use crate::process::{
    spawn_streaming, OutputChunk, RunningProcess, StreamKind, StreamWait, Utf8Decoder,
    POLL_INTERVAL,
};
use crate::{run_capture, run_capture_timeout};
use crate::{Event, EventBus, Task, TaskError, TaskResult, TaskSpec};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// A stable, UI-safe projection of a task's identity and lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: String,
    pub state: crate::TaskState,
    pub program: String,
}

#[derive(Default, Debug)]
pub struct TaskManager {
    tasks: BTreeMap<String, Task>,
}

impl TaskManager {
    pub fn clear_finished_with_events(&mut self, events: &mut EventBus) -> usize {
        let cleared = self.clear_finished();
        if cleared > 0 {
            events.publish(Event::new("task.cleaned", cleared.to_string()));
        }
        cleared
    }

    pub fn clear_finished(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|_, task| {
            matches!(
                task.state,
                crate::TaskState::Pending | crate::TaskState::Running
            )
        });
        before - self.tasks.len()
    }

    pub fn remove_with_events(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        self.remove(id)?;
        events.publish(Event::new("task.removed", id));
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<(), TaskError> {
        match self.tasks.get(id).map(|task| task.state) {
            Some(crate::TaskState::Pending) => {
                self.tasks.remove(id);
                Ok(())
            }
            Some(_) | None => Err(TaskError::InvalidTransition),
        }
    }

    /// Returns a snapshot for one task without exposing its mutable internals.
    pub fn summary(&self, id: &str) -> Option<TaskSummary> {
        self.tasks.get(id).map(|task| TaskSummary {
            id: id.to_owned(),
            state: task.state,
            program: task.spec.program.clone(),
        })
    }

    pub fn summaries(&self) -> Vec<TaskSummary> {
        self.tasks
            .iter()
            .map(|(id, task)| TaskSummary {
                id: id.clone(),
                state: task.state,
                program: task.spec.program.clone(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.tasks.contains_key(id)
    }

    pub fn state(&self, id: &str) -> Option<crate::TaskState> {
        self.tasks.get(id).map(|task| task.state)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.tasks.keys().map(String::as_str)
    }

    pub fn add(&mut self, spec: TaskSpec) -> Result<(), TaskError> {
        let id = spec.id.clone();
        if self.tasks.contains_key(&id) {
            return Err(TaskError::DuplicateId);
        }
        self.tasks.insert(id, Task::new(spec)?);
        Ok(())
    }
    pub fn start(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        let task = self.tasks.get_mut(id).ok_or(TaskError::InvalidTransition)?;
        task.start()?;
        events.publish(Event::new("task.started", id));
        Ok(())
    }
    pub fn append_output(&mut self, id: &str, chunk: &str) -> Result<(), TaskError> {
        self.tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .append_output(chunk)
    }
    pub fn append_output_with_events(
        &mut self,
        id: &str,
        chunk: &str,
        events: &mut EventBus,
    ) -> Result<(), TaskError> {
        self.append_output(id, chunk)?;
        events.publish(Event::new("task.output", format!("{id}:{chunk}")));
        Ok(())
    }

    pub fn cancel(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        self.tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .cancel()?;
        events.publish(Event::new("task.cancelled", id));
        Ok(())
    }

    pub fn run_capture(
        &mut self,
        id: &str,
        working_directory: &Path,
        events: &mut EventBus,
    ) -> io::Result<TaskResult> {
        self.start(id, events)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let output = run_capture(&spec.program, &spec.args, working_directory)?;
        let combined = output.combined();
        self.append_output_with_events(id, &combined, events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        self.finish(id, output.exit_code.unwrap_or(-1), events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    pub fn run_capture_timeout(
        &mut self,
        id: &str,
        working_directory: &Path,
        timeout: std::time::Duration,
        events: &mut EventBus,
    ) -> io::Result<TaskResult> {
        self.start(id, events)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let output = run_capture_timeout(&spec.program, &spec.args, working_directory, timeout)?;
        let combined = output.combined();
        self.append_output_with_events(id, &combined, events)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if output.exit_code.is_none() {
            let result = self
                .tasks
                .get_mut(id)
                .ok_or(TaskError::InvalidTransition)
                .and_then(|task| task.timeout())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            events.publish(Event::new("task.timeout", id));
            events.publish(Event::new("task.finished", format!("{id}:timeout")));
            Ok(result)
        } else {
            self.finish(id, output.exit_code.unwrap(), events)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
    }

    /// Streams a task's process output chunk by chunk into the task.
    ///
    /// The task is started, the process is spawned through
    /// [`crate::process::spawn_streaming`], and every chunk is decoded and
    /// appended to the task as it arrives, so `task.output` events are
    /// published incrementally instead of once at the end. The driver blocks
    /// until the process exits and then finishes the task with its exit code.
    ///
    /// Chunks are decoded incrementally per stream, so a multi-byte character
    /// split across chunk boundaries is reconstructed intact in the task's
    /// final output; `task.output` events keep the lossy per-chunk view.
    ///
    /// `on_chunk` is invoked for every chunk right after it is appended, so
    /// callers can observe output incrementally without polling the event bus.
    pub fn run_streaming<F>(
        &mut self,
        id: &str,
        working_directory: &Path,
        events: &mut EventBus,
        mut on_chunk: F,
    ) -> io::Result<TaskResult>
    where
        F: FnMut(&OutputChunk),
    {
        self.start(id, events)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let mut proc = match spawn_streaming(&spec.program, &spec.args, working_directory) {
            Ok(process) => process,
            Err(error) => {
                let _ = self.finish(id, -1, events);
                return Err(error);
            }
        };
        let mut decoders = StreamDecoders::default();
        drain_all(self, id, &mut proc, events, &mut on_chunk, &mut decoders)?;
        let output = proc.wait()?;
        flush_decoders(self, id, &mut decoders)?;
        self.finish(id, output.exit_code.unwrap_or(-1), events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    /// Like [`TaskManager::run_streaming`], but capped by a hard deadline.
    ///
    /// If the process has not exited once `timeout` has elapsed since it was
    /// spawned, the process is killed, any still-buffered output is drained
    /// into the task, and the task is marked [`crate::TaskState::TimedOut`],
    /// publishing `task.timeout` and `task.finished` (`{id}:timeout`) events.
    /// The deadline applies even if the streams reached EOF early: a process
    /// that closed its stdout/stderr without exiting is still subject to it.
    /// Otherwise the task finishes normally with the process's exit code.
    pub fn run_streaming_timeout<F>(
        &mut self,
        id: &str,
        working_directory: &Path,
        timeout: std::time::Duration,
        events: &mut EventBus,
        mut on_chunk: F,
    ) -> io::Result<TaskResult>
    where
        F: FnMut(&OutputChunk),
    {
        self.start(id, events)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let mut proc = match spawn_streaming(&spec.program, &spec.args, working_directory) {
            Ok(process) => process,
            Err(error) => {
                let _ = self.finish(id, -1, events);
                return Err(error);
            }
        };
        let started = std::time::Instant::now();
        let mut timed_out = false;
        let mut streams_closed = false;
        let mut decoders = StreamDecoders::default();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                timed_out = true;
                break;
            }
            let remaining = timeout - elapsed;
            if streams_closed {
                // Both streams reached EOF, but the child may still be
                // running (e.g. it closed its stdout/stderr without
                // exiting). Keep honoring the deadline by polling the child
                // itself; otherwise wait() here would block until the process
                // exits on its own — possibly forever.
                if proc.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(remaining.min(POLL_INTERVAL));
                continue;
            }
            match proc.next_chunk_timeout(remaining)? {
                StreamWait::Chunk(chunk) => {
                    append_chunk(self, id, &chunk, &mut decoders, events)?;
                    on_chunk(&chunk);
                }
                StreamWait::Closed => streams_closed = true,
                StreamWait::Timeout => {
                    timed_out = true;
                    break;
                }
            }
        }
        if timed_out {
            let _ = proc.kill();
            // Drain whatever the reader threads had buffered so the task keeps
            // every byte the process produced before it was killed.
            drain_all(self, id, &mut proc, events, &mut on_chunk, &mut decoders)?;
            let _ = proc.wait()?;
            flush_decoders(self, id, &mut decoders)?;
            let result = self
                .tasks
                .get_mut(id)
                .ok_or(TaskError::InvalidTransition)
                .and_then(|task| task.timeout())
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
            events.publish(Event::new("task.timeout", id));
            events.publish(Event::new("task.finished", format!("{id}:timeout")));
            Ok(result)
        } else {
            let output = proc.wait()?;
            flush_decoders(self, id, &mut decoders)?;
            self.finish(id, output.exit_code.unwrap_or(-1), events)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
        }
    }

    pub fn finish(
        &mut self,
        id: &str,
        code: i32,
        events: &mut EventBus,
    ) -> Result<TaskResult, TaskError> {
        let result = self
            .tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .finish(code)?;
        if code != 0 {
            events.publish(Event::new("task.failed", format!("{id}:{code}")));
        }
        events.publish(Event::new("task.finished", format!("{id}:{code}")));
        Ok(result)
    }
}

/// Per-stream incremental UTF-8 decoders used while merging stdout and stderr
/// chunks into a task's single output buffer.
///
/// Each stream keeps its own decoder state so a multi-byte character split
/// across chunks is reconstructed exactly once, and bytes from the two
/// streams are never joined into one character.
#[derive(Default, Debug)]
struct StreamDecoders {
    stdout: Utf8Decoder,
    stderr: Utf8Decoder,
}

impl StreamDecoders {
    fn for_kind(&mut self, kind: StreamKind) -> &mut Utf8Decoder {
        match kind {
            StreamKind::Stdout => &mut self.stdout,
            StreamKind::Stderr => &mut self.stderr,
        }
    }
}

fn append_chunk(
    manager: &mut TaskManager,
    id: &str,
    chunk: &OutputChunk,
    decoders: &mut StreamDecoders,
    events: &mut EventBus,
) -> io::Result<()> {
    // Decode incrementally per stream so a multi-byte character split across
    // chunk boundaries lands intact in the task's final output instead of as
    // replacement characters.
    let text = decoders.for_kind(chunk.kind).feed(&chunk.data);
    manager
        .append_output(id, &text)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    // `task.output` events keep the lossy per-chunk view; `on_chunk` already
    // hands callers the raw bytes to decode however they need.
    events.publish(Event::new("task.output", format!("{id}:{}", chunk.text())));
    Ok(())
}

/// Flushes any bytes the decoders still hold — a multi-byte character that
/// was genuinely truncated at end of stream — into the task, lossily.
fn flush_decoders(
    manager: &mut TaskManager,
    id: &str,
    decoders: &mut StreamDecoders,
) -> io::Result<()> {
    let tail = format!("{}{}", decoders.stdout.finish(), decoders.stderr.finish());
    if !tail.is_empty() {
        manager
            .append_output(id, &tail)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    }
    Ok(())
}

fn drain_all<F>(
    manager: &mut TaskManager,
    id: &str,
    proc: &mut RunningProcess,
    events: &mut EventBus,
    on_chunk: &mut F,
    decoders: &mut StreamDecoders,
) -> io::Result<()>
where
    F: FnMut(&OutputChunk),
{
    while let Some(chunk) = proc.next_chunk()? {
        append_chunk(manager, id, &chunk, decoders, events)?;
        on_chunk(&chunk);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_runs_real_process_and_captures_output() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo task".into()])
        } else {
            ("sh", vec!["-c".into(), "printf task".into()])
        };
        manager
            .add(TaskSpec {
                id: "echo".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let result = manager
            .run_capture("echo", Path::new("."), &mut events)
            .unwrap();
        assert_eq!(result.state, crate::TaskState::Succeeded);
        assert!(result.output.contains("task"));
    }

    #[test]
    fn manager_exposes_task_state_and_ids() {
        let mut manager = TaskManager::default();
        manager
            .add(TaskSpec {
                id: "build".into(),
                program: "echo".into(),
                args: vec![],
            })
            .unwrap();
        assert_eq!(
            manager.summaries()[0],
            TaskSummary {
                id: "build".into(),
                state: crate::TaskState::Pending,
                program: "echo".into()
            }
        );
        assert_eq!(manager.len(), 1);
        assert!(!manager.is_empty());
        assert!(manager.contains("build"));
        assert!(!manager.contains("missing"));
        assert_eq!(manager.state("build"), Some(crate::TaskState::Pending));
        assert_eq!(manager.state("missing"), None);
        assert_eq!(manager.ids().collect::<Vec<_>>(), vec!["build"]);
        let mut removal_events = EventBus::default();
        manager
            .remove_with_events("build", &mut removal_events)
            .unwrap();
        assert_eq!(removal_events.drain("task.removed")[0].payload, "build");
        assert!(manager.ids().next().is_none());
        assert_eq!(manager.remove("missing"), Err(TaskError::InvalidTransition));
        assert_eq!(manager.clear_finished(), 0);
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "done".into(),
                program: "echo".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("done", &mut events).unwrap();
        manager.finish("done", 0, &mut events).unwrap();
        assert_eq!(manager.clear_finished_with_events(&mut events), 1);
        assert_eq!(manager.state("done"), None);
        assert_eq!(events.drain("task.cleaned")[0].payload, "1");
    }

    #[test]
    fn running_tasks_cannot_be_removed() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "running".into(),
                program: "echo".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("running", &mut events).unwrap();
        assert_eq!(
            manager.remove_with_events("running", &mut events),
            Err(TaskError::InvalidTransition)
        );
        assert!(events.drain("task.removed").is_empty());
    }

    #[test]
    fn manager_emits_task_lifecycle_events() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "build".into(),
                program: "cargo".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("build", &mut events).unwrap();
        manager.append_output("build", "ok").unwrap();
        let result = manager.finish("build", 0, &mut events).unwrap();
        assert_eq!(result.output, "ok");
        assert_eq!(events.drain("task.started")[0].payload, "build");
        assert_eq!(events.drain("task.finished")[0].payload, "build:0");
    }

    #[test]
    fn streaming_spawn_failure_finishes_task() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "missing".into(),
                program: "nextide-program-that-does-not-exist".into(),
                args: vec![],
            })
            .unwrap();
        assert!(manager
            .run_streaming("missing", Path::new("."), &mut events, |_| {})
            .is_err());
        assert_eq!(manager.state("missing"), Some(crate::TaskState::Failed));
    }

    #[test]
    fn manager_streams_process_output_chunk_by_chunk() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec!["/C".into(), "echo out && echo err 1>&2".into()],
            )
        } else {
            ("sh", vec!["-c".into(), "printf out; printf err >&2".into()])
        };
        manager
            .add(TaskSpec {
                id: "stream".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let mut seen = Vec::new();
        let result = manager
            .run_streaming("stream", Path::new("."), &mut events, |chunk| {
                seen.push(chunk.kind);
            })
            .unwrap();
        assert_eq!(result.state, crate::TaskState::Succeeded);
        assert!(
            result.output.contains("out"),
            "output was {:?}",
            result.output
        );
        assert!(
            result.output.contains("err"),
            "output was {:?}",
            result.output
        );
        assert!(seen.contains(&crate::process::StreamKind::Stdout));
        assert!(seen.contains(&crate::process::StreamKind::Stderr));
        // Chunks were appended incrementally: at least one task.output event
        // fired and the task finished with the real exit code.
        assert!(!events.drain("task.output").is_empty());
        assert_eq!(events.drain("task.finished")[0].payload, "stream:0");
        assert_eq!(manager.state("stream"), Some(crate::TaskState::Succeeded));
    }

    #[test]
    fn manager_streaming_timeout_kills_and_marks_timed_out() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("ping", vec!["127.0.0.1".into(), "-n".into(), "5".into()])
        } else {
            ("sh", vec!["-c".into(), "sleep 30".into()])
        };
        manager
            .add(TaskSpec {
                id: "slow".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let result = manager
            .run_streaming_timeout(
                "slow",
                Path::new("."),
                std::time::Duration::from_millis(100),
                &mut events,
                |_| {},
            )
            .unwrap();
        assert_eq!(result.state, crate::TaskState::TimedOut);
        assert_eq!(result.exit_code, None);
        assert_eq!(events.drain("task.timeout")[0].payload, "slow");
        assert_eq!(events.drain("task.finished")[0].payload, "slow:timeout");
        // The killed process was reaped, so the task is still addressable.
        assert_eq!(manager.state("slow"), Some(crate::TaskState::TimedOut));
    }

    #[cfg(not(windows))]
    #[test]
    fn streaming_timeout_still_applies_when_streams_close_early() {
        // The child closes its stdout/stderr immediately (so the stream
        // channel reports Closed) but keeps running for 30s. The deadline must
        // still apply instead of blocking on wait() until the process exits on
        // its own.
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = silent_linger_command();
        manager
            .add(TaskSpec {
                id: "silent".into(),
                program,
                args,
            })
            .unwrap();
        let (result, manager, mut events) =
            with_deadline(std::time::Duration::from_secs(15), || {
                let result = manager.run_streaming_timeout(
                    "silent",
                    Path::new("."),
                    std::time::Duration::from_millis(150),
                    &mut events,
                    |_| {},
                );
                (result, manager, events)
            })
            .expect("run_streaming_timeout hung: streams closed but the process kept running");
        let result = result.unwrap();
        assert_eq!(result.state, crate::TaskState::TimedOut);
        assert_eq!(result.exit_code, None);
        assert_eq!(events.drain("task.timeout")[0].payload, "silent");
        assert_eq!(events.drain("task.finished")[0].payload, "silent:timeout");
        assert_eq!(manager.state("silent"), Some(crate::TaskState::TimedOut));
    }

    #[test]
    fn manager_streaming_finishes_within_timeout() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo done".into()])
        } else {
            ("sh", vec!["-c".into(), "printf done".into()])
        };
        manager
            .add(TaskSpec {
                id: "quick".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let result = manager
            .run_streaming_timeout(
                "quick",
                Path::new("."),
                std::time::Duration::from_secs(30),
                &mut events,
                |_| {},
            )
            .unwrap();
        assert_eq!(result.state, crate::TaskState::Succeeded);
        assert!(
            result.output.contains("done"),
            "output was {:?}",
            result.output
        );
        assert!(events.drain("task.timeout").is_empty());
    }

    #[test]
    fn streaming_reconstructs_multibyte_characters_split_across_chunks() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "utf8".into(),
                program: "sh".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("utf8", &mut events).unwrap();
        let mut decoders = StreamDecoders::default();
        // "héllo": "é" (U+00E9, bytes C3 A9) is split across the two chunks.
        append_chunk(
            &mut manager,
            "utf8",
            &OutputChunk {
                kind: StreamKind::Stdout,
                data: b"h\xc3".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        append_chunk(
            &mut manager,
            "utf8",
            &OutputChunk {
                kind: StreamKind::Stdout,
                data: b"\xa9llo".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        flush_decoders(&mut manager, "utf8", &mut decoders).unwrap();
        let result = manager.finish("utf8", 0, &mut events).unwrap();
        assert_eq!(result.output, "héllo");
        assert!(
            !result.output.contains('\u{FFFD}'),
            "split character leaked a replacement char into the final output"
        );
        // `task.output` events keep the lossy per-chunk view: the split
        // character shows up as replacement characters in the event stream
        // even though the task's final output is exact.
        let events = events.drain("task.output");
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.payload.contains('\u{FFFD}')),
            "events were expected to stay lossy per chunk"
        );
    }

    #[test]
    fn streaming_keeps_stdout_and_stderr_decoding_independent() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "split".into(),
                program: "sh".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("split", &mut events).unwrap();
        let mut decoders = StreamDecoders::default();
        // "你" (bytes E4 BD A0) is split across two stdout chunks with a
        // stderr chunk arriving in between; the two streams must never be
        // joined into one character.
        append_chunk(
            &mut manager,
            "split",
            &OutputChunk {
                kind: StreamKind::Stdout,
                data: b"\xe4\xbd".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        append_chunk(
            &mut manager,
            "split",
            &OutputChunk {
                kind: StreamKind::Stderr,
                data: b"err".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        append_chunk(
            &mut manager,
            "split",
            &OutputChunk {
                kind: StreamKind::Stdout,
                data: b"\xa0".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        flush_decoders(&mut manager, "split", &mut decoders).unwrap();
        let result = manager.finish("split", 0, &mut events).unwrap();
        assert_eq!(result.output, "err你");
        assert!(!result.output.contains('\u{FFFD}'));
    }

    #[test]
    fn streaming_flushes_genuinely_truncated_tail_lossily() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "cut".into(),
                program: "sh".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("cut", &mut events).unwrap();
        let mut decoders = StreamDecoders::default();
        // The stream genuinely ends in the middle of "é": the tail must be
        // flushed as a replacement character, not silently dropped.
        append_chunk(
            &mut manager,
            "cut",
            &OutputChunk {
                kind: StreamKind::Stdout,
                data: b"ab\xc3".to_vec(),
            },
            &mut decoders,
            &mut events,
        )
        .unwrap();
        flush_decoders(&mut manager, "cut", &mut decoders).unwrap();
        let result = manager.finish("cut", 0, &mut events).unwrap();
        assert_eq!(result.output, "ab\u{FFFD}");
    }

    #[cfg(not(windows))]
    #[test]
    fn streaming_recovers_characters_split_across_real_chunk_boundaries() {
        // Emit 8191 ASCII bytes followed by a 2-byte character in one write
        // so the 8192-byte reader buffer splits the character across two
        // chunks. The task's final output must reconstruct it exactly.
        let payload = format!("{}é", "a".repeat(8191));
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "wide".into(),
                program: "sh".into(),
                args: vec!["-c".into(), format!("printf '%s' '{}'", payload)],
            })
            .unwrap();
        let result = manager
            .run_streaming("wide", Path::new("."), &mut events, |_| {})
            .unwrap();
        assert_eq!(result.state, crate::TaskState::Succeeded);
        assert_eq!(result.output, payload);
        assert!(
            !result.output.contains('\u{FFFD}'),
            "output was {:?}",
            result.output
        );
    }

    /// Runs `f` on a helper thread and returns `None` if it does not finish
    /// within `deadline`, so a regression that blocks forever fails the test
    /// instead of hanging the whole suite.
    #[cfg(not(windows))]
    fn with_deadline<T: Send + 'static>(
        deadline: std::time::Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(f());
        });
        receiver.recv_timeout(deadline).ok()
    }

    /// A command whose child closes both stdout and stderr immediately but
    /// keeps running for 30s: the stream channel reports [`StreamWait::Closed`]
    /// while the process is still alive. `exec 1>&- 2>&-` closes the shell's
    /// own fds, so the pipe write ends are gone even though the child keeps
    /// running (`sleep` inherits the closed descriptors).
    ///
    /// Windows cannot reproduce this with stock tools: closing the standard
    /// handles from inside a process (PowerShell/.NET) leaves duplicate
    /// references to the pipe write ends behind, so the streams never report
    /// Closed while the child lives.
    #[cfg(not(windows))]
    fn silent_linger_command() -> (String, Vec<String>) {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "exec 1>&- 2>&-; sleep 30".to_string()],
        )
    }
}

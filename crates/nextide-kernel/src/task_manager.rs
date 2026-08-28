use crate::process::{spawn_streaming, OutputChunk, RunningProcess, StreamWait};
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
    /// [`crate::process::spawn_streaming`], and every chunk is appended via
    /// [`TaskManager::append_output_with_events`] as it arrives, so `task.output`
    /// events are published incrementally instead of once at the end. The
    /// driver blocks until the process exits and then finishes the task with
    /// its exit code.
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
        drain_all(self, id, &mut proc, events, &mut on_chunk)?;
        let output = proc.wait()?;
        self.finish(id, output.exit_code.unwrap_or(-1), events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    /// Like [`TaskManager::run_streaming`], but capped by a hard deadline.
    ///
    /// If no further chunk arrives (and the streams are not yet closed) once
    /// `timeout` has elapsed since the process was spawned, the process is
    /// killed, any still-buffered output is drained into the task, and the task
    /// is marked [`crate::TaskState::TimedOut`], publishing `task.timeout` and
    /// `task.finished` (`{id}:timeout`) events. Otherwise the task finishes
    /// normally with the process's exit code.
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
        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                timed_out = true;
                break;
            }
            match proc.next_chunk_timeout(timeout - elapsed)? {
                StreamWait::Chunk(chunk) => {
                    append_chunk(self, id, &chunk, events)?;
                    on_chunk(&chunk);
                }
                StreamWait::Closed => break,
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
            drain_all(self, id, &mut proc, events, &mut on_chunk)?;
            let _ = proc.wait()?;
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

fn append_chunk(
    manager: &mut TaskManager,
    id: &str,
    chunk: &OutputChunk,
    events: &mut EventBus,
) -> io::Result<()> {
    manager
        .append_output_with_events(id, &chunk.text(), events)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
}

fn drain_all<F>(
    manager: &mut TaskManager,
    id: &str,
    proc: &mut RunningProcess,
    events: &mut EventBus,
    on_chunk: &mut F,
) -> io::Result<()>
where
    F: FnMut(&OutputChunk),
{
    while let Some(chunk) = proc.next_chunk()? {
        append_chunk(manager, id, &chunk, events)?;
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
}

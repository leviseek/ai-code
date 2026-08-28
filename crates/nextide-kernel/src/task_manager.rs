use crate::async_task::{AsyncTaskHandle, AsyncTaskState, AsyncTaskStatus};
use crate::process::{
    spawn_streaming, OutputChunk, RunningProcess, StreamKind, StreamWait, Utf8Decoder,
    POLL_INTERVAL,
};
use crate::{run_capture, run_capture_timeout};
use crate::{Event, EventBus, Task, TaskError, TaskResult, TaskSpec};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// A stable, UI-safe projection of a task's identity and lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: String,
    pub state: crate::TaskState,
    pub program: String,
}

#[derive(Default, Debug)]
pub struct TaskManager {
    /// Each task lives behind a shared mutex so a background worker can keep
    /// driving its record (output, terminal state) while the manager's
    /// synchronous API keeps operating on it from the caller's thread. The
    /// public API is unchanged; the lock is only ever held briefly.
    tasks: BTreeMap<String, Arc<Mutex<Task>>>,
    /// Cancellation flags of tasks spawned through [`TaskManager::spawn_async`]
    /// / [`TaskManager::spawn_async_timeout`], keyed by task id and shared with
    /// the returned [`AsyncTaskHandle`] and its worker thread. A
    /// [`TaskManager::cancel`] flips the flag so the worker kills the process
    /// and drives both the handle and the manager record to `Cancelled`;
    /// without it the record would say `Cancelled` while the handle stayed
    /// `Running` and the process kept going. Entries are dropped when their
    /// task is removed or cleaned up, mirroring the task records themselves.
    async_cancels: BTreeMap<String, Arc<AtomicBool>>,
}

/// Locks a task's shared record, recovering from a poisoned mutex so a
/// panicking worker thread cannot take down the manager's API surface.
fn lock_task(task: &Arc<Mutex<Task>>) -> MutexGuard<'_, Task> {
    task.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
                lock_task(task).state,
                crate::TaskState::Pending | crate::TaskState::Running
            )
        });
        // Drop the async cancel flags of cleaned-up tasks so a later task
        // reusing the id starts with a fresh, unset flag.
        self.async_cancels
            .retain(|id, _| self.tasks.contains_key(id));
        before - self.tasks.len()
    }

    pub fn remove_with_events(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        self.remove(id)?;
        events.publish(Event::new("task.removed", id));
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<(), TaskError> {
        match self.tasks.get(id).map(|task| lock_task(task).state) {
            Some(crate::TaskState::Pending) => {
                self.tasks.remove(id);
                self.async_cancels.remove(id);
                Ok(())
            }
            Some(_) | None => Err(TaskError::InvalidTransition),
        }
    }

    /// Returns a snapshot for one task without exposing its mutable internals.
    pub fn summary(&self, id: &str) -> Option<TaskSummary> {
        self.tasks.get(id).map(|task| {
            let task = lock_task(task);
            TaskSummary {
                id: id.to_owned(),
                state: task.state,
                program: task.spec.program.clone(),
            }
        })
    }

    pub fn summaries(&self) -> Vec<TaskSummary> {
        self.tasks
            .iter()
            .map(|(id, task)| {
                let task = lock_task(task);
                TaskSummary {
                    id: id.clone(),
                    state: task.state,
                    program: task.spec.program.clone(),
                }
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
        self.tasks.get(id).map(|task| lock_task(task).state)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.tasks.keys().map(String::as_str)
    }

    pub fn add(&mut self, spec: TaskSpec) -> Result<(), TaskError> {
        let id = spec.id.clone();
        if self.tasks.contains_key(&id) {
            return Err(TaskError::DuplicateId);
        }
        self.tasks
            .insert(id, Arc::new(Mutex::new(Task::new(spec)?)));
        Ok(())
    }
    pub fn start(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        let task = self.tasks.get(id).ok_or(TaskError::InvalidTransition)?;
        lock_task(task).start()?;
        events.publish(Event::new("task.started", id));
        Ok(())
    }
    pub fn append_output(&mut self, id: &str, chunk: &str) -> Result<(), TaskError> {
        let task = self.tasks.get(id).ok_or(TaskError::InvalidTransition)?;
        lock_task(task).append_output(chunk)
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

    /// Cancels a task, killing the background process when the task was started
    /// with [`TaskManager::spawn_async`] / [`TaskManager::spawn_async_timeout`].
    ///
    /// The manager record transitions to [`crate::TaskState::Cancelled`] and —
    /// for async tasks — the handle's shared cancellation flag is set *first*,
    /// so the worker kills the process and drives the handle to
    /// [`AsyncTaskStatus::Cancelled`] as well: the two views converge instead
    /// of the process outliving the cancellation and the handle landing on
    /// `Succeeded`/`Failed` while the record says `Cancelled`. A task that
    /// already reached a terminal state is rejected exactly as before.
    pub fn cancel(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        let task = self.tasks.get(id).ok_or(TaskError::InvalidTransition)?;
        // Signal the async worker before transitioning the record, so a worker
        // that is about to finish naturally still converges on Cancelled
        // instead of letting the handle and the record disagree.
        if let Some(flag) = self.async_cancels.get(id) {
            flag.store(true, Ordering::SeqCst);
        }
        lock_task(task).cancel()?;
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
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?;
        let spec = lock_task(task).spec.clone();
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
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?;
        let spec = lock_task(task).spec.clone();
        let output = run_capture_timeout(&spec.program, &spec.args, working_directory, timeout)?;
        let combined = output.combined();
        self.append_output_with_events(id, &combined, events)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if output.exit_code.is_none() {
            let result = self
                .tasks
                .get(id)
                .ok_or(TaskError::InvalidTransition)
                .map(lock_task)
                .and_then(|mut task| task.timeout())
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
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?;
        let spec = lock_task(task).spec.clone();
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
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?;
        let spec = lock_task(task).spec.clone();
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
                .get(id)
                .ok_or(TaskError::InvalidTransition)
                .map(lock_task)
                .and_then(|mut task| task.timeout())
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
        let task = self.tasks.get(id).ok_or(TaskError::InvalidTransition)?;
        let result = lock_task(task).finish(code)?;
        if code != 0 {
            events.publish(Event::new("task.failed", format!("{id}:{code}")));
        }
        events.publish(Event::new("task.finished", format!("{id}:{code}")));
        Ok(result)
    }

    /// Starts a task in the background and returns a pollable handle.
    ///
    /// Unlike the synchronous [`TaskManager::run_streaming`], this method
    /// returns as soon as the process has been spawned: the process is driven
    /// by a dedicated worker thread that streams output into the handle and
    /// the manager's task record while the caller keeps running. Poll the
    /// handle with [`AsyncTaskHandle::poll`] /
    /// [`AsyncTaskHandle::drain_output`] and block on completion with
    /// [`AsyncTaskHandle::wait`] / [`AsyncTaskHandle::wait_timeout`], or
    /// request termination with [`AsyncTaskHandle::cancel`] — which the
    /// manager-side [`TaskManager::cancel`] does too, killing the background
    /// process and driving both the handle and the manager record to
    /// `Cancelled`.
    ///
    /// The task is started (and `task.started` is published) synchronously,
    /// exactly like the sync run methods. Output is decoded incrementally per
    /// stream, so a multi-byte character split across chunk boundaries is
    /// reconstructed intact, and stdout/stderr are merged in arrival order —
    /// the same semantics as [`TaskManager::run_streaming`]. Completion is
    /// observed through the handle rather than `task.output`/`task.finished`
    /// events, because the worker cannot publish into a caller-held
    /// [`EventBus`].
    ///
    /// A spawn failure (e.g. the program does not exist) is reported
    /// synchronously and the task is finished as failed, mirroring
    /// [`TaskManager::run_streaming`]. If the worker thread cannot be created,
    /// the spawned process is killed and the task is finished as failed.
    pub fn spawn_async(
        &mut self,
        id: &str,
        working_directory: &Path,
        events: &mut EventBus,
    ) -> io::Result<AsyncTaskHandle> {
        self.spawn_async_inner(id, working_directory, None, events)
    }

    /// Like [`TaskManager::spawn_async`], but capped by a hard deadline.
    ///
    /// If the process has not exited once `timeout` has elapsed since it was
    /// spawned, the worker kills it (and its descendants), drains any
    /// still-buffered output into the handle, and marks the task
    /// [`AsyncTaskStatus::TimedOut`] / [`crate::TaskState::TimedOut`]. The
    /// deadline applies even if the streams reached EOF early.
    pub fn spawn_async_timeout(
        &mut self,
        id: &str,
        working_directory: &Path,
        timeout: std::time::Duration,
        events: &mut EventBus,
    ) -> io::Result<AsyncTaskHandle> {
        self.spawn_async_inner(id, working_directory, Some(timeout), events)
    }

    fn spawn_async_inner(
        &mut self,
        id: &str,
        working_directory: &Path,
        timeout: Option<std::time::Duration>,
        events: &mut EventBus,
    ) -> io::Result<AsyncTaskHandle> {
        self.start(id, events)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .clone();
        let spec = lock_task(&task).spec.clone();
        let proc = match spawn_streaming(&spec.program, &spec.args, working_directory) {
            Ok(process) => process,
            Err(error) => {
                // Mirrors run_streaming: a task whose process cannot be
                // spawned is finished as failed.
                let _ = self.finish(id, -1, events);
                return Err(error);
            }
        };
        let handle = AsyncTaskHandle::new(id);
        // Register the handle's cancellation flag so TaskManager::cancel can
        // stop this background worker; the worker itself holds a clone in
        // AsyncTaskShared. Removed again when the task is cleaned up.
        self.async_cancels
            .insert(id.to_owned(), handle.cancelled.clone());
        let shared = AsyncTaskShared {
            task,
            state: handle.state.clone(),
            done: handle.done.clone(),
            cancelled: handle.cancelled.clone(),
        };
        match std::thread::Builder::new()
            .name(format!("nextide-task-{id}"))
            .spawn(move || run_async_worker(shared, proc, timeout))
        {
            Ok(_) => Ok(handle),
            Err(error) => {
                // Dropping the closure drops `proc`, which kills and reaps the
                // child; finish the task as failed so the manager stays
                // consistent, and drop the now-unused cancel flag.
                self.async_cancels.remove(id);
                let _ = self.finish(id, -1, events);
                Err(io::Error::other(error))
            }
        }
    }
}

/// Everything the background worker needs to drive one async task: a clone of
/// the manager's task record (so the manager's synchronous views stay
/// consistent), the handle's shared state (so pollers see live output and
/// status), and the cancellation and completion signals.
struct AsyncTaskShared {
    task: Arc<Mutex<Task>>,
    state: Arc<Mutex<AsyncTaskState>>,
    done: Arc<(Mutex<bool>, Condvar)>,
    cancelled: Arc<AtomicBool>,
}

/// Runs the async worker's body so a panic can never leave the handle stuck in
/// [`AsyncTaskStatus::Running`]: on unwind the shared state is driven to
/// `Failed` and waiters are woken, then the panic is re-raised so it stays
/// observable. The dropped [`RunningProcess`] (unwound inside the body) has
/// already killed and reaped the child, so nothing leaks and
/// [`TaskManager::clear_finished`] can still clean the task up.
fn run_worker_safely(shared: &AsyncTaskShared, body: impl FnOnce()) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    if let Err(payload) = outcome {
        finish_async(shared, AsyncTaskStatus::Failed { exit_code: -1 });
        std::panic::resume_unwind(payload);
    }
}

/// Drives a spawned process in the background until it exits, times out, or is
/// cancelled, feeding decoded output into both the handle's shared state and
/// the manager's task record, then transitioning the task to its terminal
/// state. The worker never blocks a poller: all shared access goes through the
/// handle's mutexes, which are only ever held briefly.
///
/// The body runs inside [`run_worker_safely`], so even a panicking worker
/// converges the handle and the manager record onto a terminal state instead
/// of leaving the handle `Running` forever.
fn run_async_worker(
    shared: AsyncTaskShared,
    mut proc: RunningProcess,
    timeout: Option<std::time::Duration>,
) {
    run_worker_safely(&shared, || {
        run_async_worker_inner(&shared, &mut proc, timeout);
    });
}

fn run_async_worker_inner(
    shared: &AsyncTaskShared,
    proc: &mut RunningProcess,
    timeout: Option<std::time::Duration>,
) {
    let started = std::time::Instant::now();
    let mut decoders = StreamDecoders::default();
    let mut streams_closed = false;
    let mut timed_out = false;
    loop {
        if shared.cancelled.load(Ordering::SeqCst) {
            let _ = proc.kill();
            drain_async_output(proc, &mut decoders, shared);
            let _ = proc.wait();
            flush_async_decoders(shared, &mut decoders);
            finish_async(shared, AsyncTaskStatus::Cancelled);
            return;
        }
        let wait = match timeout {
            Some(deadline) => {
                let elapsed = started.elapsed();
                if elapsed >= deadline {
                    timed_out = true;
                    break;
                }
                let remaining = deadline - elapsed;
                if streams_closed {
                    // Both streams reached EOF, but the child may still be
                    // running: poll the child itself until the deadline, as
                    // the synchronous timeout path does.
                    match proc.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            std::thread::sleep(remaining.min(POLL_INTERVAL));
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                proc.next_chunk_timeout(remaining)
            }
            None => {
                if streams_closed {
                    match proc.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            std::thread::sleep(POLL_INTERVAL);
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                // Poll with a short slice so cancellation is observed
                // promptly even when no output arrives.
                proc.next_chunk_timeout(POLL_INTERVAL)
            }
        };
        match wait {
            Ok(StreamWait::Chunk(chunk)) => {
                append_async_chunk(&chunk, &mut decoders, shared);
            }
            Ok(StreamWait::Closed) => streams_closed = true,
            Ok(StreamWait::Timeout) => {
                if timeout.is_some() {
                    timed_out = true;
                    break;
                }
                // No deadline: nothing arrived; loop back to re-check
                // cancellation.
            }
            Err(_) => break,
        }
    }
    if timed_out {
        let _ = proc.kill();
        // Drain whatever the reader threads had buffered so the handle keeps
        // every byte the process produced before it was killed.
        drain_async_output(proc, &mut decoders, shared);
        let _ = proc.wait();
        flush_async_decoders(shared, &mut decoders);
        finish_async(shared, AsyncTaskStatus::TimedOut);
    } else {
        let exit_code = proc
            .wait()
            .ok()
            .and_then(|output| output.exit_code)
            .unwrap_or(-1);
        flush_async_decoders(shared, &mut decoders);
        let status = if exit_code == 0 {
            AsyncTaskStatus::Succeeded { exit_code }
        } else {
            AsyncTaskStatus::Failed { exit_code }
        };
        finish_async(shared, status);
    }
}

/// Decodes one chunk through its stream's decoder and appends the text to the
/// handle's shared output and the manager's task record, mirroring the
/// synchronous streaming path (minus the event publication).
fn append_async_chunk(
    chunk: &OutputChunk,
    decoders: &mut StreamDecoders,
    shared: &AsyncTaskShared,
) {
    let text = decoders.for_kind(chunk.kind).feed(&chunk.data);
    append_async_text(shared, &text);
}

/// Appends decoded text to the handle's shared output and, best effort, to the
/// manager's task record. A failed task-record append (e.g. the task was
/// already cancelled) is not fatal: the handle remains the source of truth for
/// async output.
fn append_async_text(shared: &AsyncTaskShared, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(mut state) = shared.state.lock() {
        state.output.push_str(text);
    }
    if let Ok(mut task) = shared.task.lock() {
        let _ = task.append_output(text);
    }
}

/// Flushes any bytes the decoders still hold — a multi-byte character that was
/// genuinely truncated at end of stream — into the handle and the task,
/// lossily.
fn flush_async_decoders(shared: &AsyncTaskShared, decoders: &mut StreamDecoders) {
    let tail = format!("{}{}", decoders.stdout.finish(), decoders.stderr.finish());
    append_async_text(shared, &tail);
}

/// Drains whatever the reader threads buffered (e.g. after a kill) into the
/// handle's shared output and the manager's task record.
fn drain_async_output(
    proc: &mut RunningProcess,
    decoders: &mut StreamDecoders,
    shared: &AsyncTaskShared,
) {
    while let Ok(Some(chunk)) = proc.next_chunk() {
        append_async_chunk(&chunk, decoders, shared);
    }
}

/// Records the terminal status in the handle's shared state, transitions the
/// manager's task record to match, and wakes every waiter.
///
/// The shared cancellation flag is authoritative: once cancellation has been
/// requested — through either [`AsyncTaskHandle::cancel`] or
/// [`TaskManager::cancel`] — the task ends `Cancelled` even if the process
/// happened to exit around the same time, so the handle and the manager record
/// can never diverge (one landing on `Succeeded`/`Failed`/`TimedOut` while the
/// other says `Cancelled`).
fn finish_async(shared: &AsyncTaskShared, status: AsyncTaskStatus) {
    let status = if shared.cancelled.load(Ordering::SeqCst) {
        AsyncTaskStatus::Cancelled
    } else {
        status
    };
    if let Ok(mut state) = shared.state.lock() {
        state.status = status.clone();
    }
    if let Ok(mut task) = shared.task.lock() {
        match &status {
            AsyncTaskStatus::Succeeded { exit_code } | AsyncTaskStatus::Failed { exit_code } => {
                // Retain the output: the sync run methods hand the drained
                // TaskResult to their caller, but the async caller reads output
                // through the handle, so draining it here would silently wipe
                // the manager's task record.
                let _ = task.finish_retain_output(*exit_code);
            }
            AsyncTaskStatus::TimedOut => {
                let _ = task.timeout_retain_output();
            }
            AsyncTaskStatus::Cancelled => {
                let _ = task.cancel();
            }
            AsyncTaskStatus::Running => {}
        }
    }
    let (lock, cvar) = &*shared.done;
    *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    cvar.notify_all();
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

    /// A command that prints "a", pauses, then prints "b": long enough for a
    /// poller to observe intermediate output while the task is still running.
    fn async_staggered_command() -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec![
                    "/C".to_string(),
                    "echo a & ping -n 2 127.0.0.1 >nul & echo b".to_string(),
                ],
            )
        } else {
            (
                "sh".to_string(),
                vec![
                    "-c".to_string(),
                    "printf a; sleep 0.3; printf b".to_string(),
                ],
            )
        }
    }

    #[test]
    fn spawn_async_polls_intermediate_output_and_completion() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = async_staggered_command();
        manager
            .add(TaskSpec {
                id: "async".into(),
                program,
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("async", Path::new("."), &mut events)
            .unwrap();
        // The task is started synchronously, exactly like the sync run methods.
        assert_eq!(events.drain("task.started")[0].payload, "async");
        // The handle is pollable immediately and never blocks the caller.
        let snapshot = handle.poll();
        assert_eq!(snapshot.id, "async");
        assert_eq!(snapshot.status, AsyncTaskStatus::Running);
        // A second spawn while the task is still running must fail.
        assert!(manager
            .spawn_async("async", Path::new("."), &mut events)
            .is_err());
        // Poll until the first chunk of output arrives while still running.
        let started = std::time::Instant::now();
        let mut seen = String::new();
        while handle.is_running() && started.elapsed() < std::time::Duration::from_secs(5) {
            seen.push_str(&handle.drain_output());
            if seen.contains('a') {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            seen.contains('a'),
            "intermediate output never arrived while running; saw {:?}",
            seen
        );
        // Block until completion and read the final snapshot.
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("async task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::Succeeded { exit_code: 0 });
        assert!(
            snapshot.output.contains('a'),
            "output was {:?}",
            snapshot.output
        );
        assert!(
            snapshot.output.contains('b'),
            "output was {:?}",
            snapshot.output
        );
        assert!(!handle.is_running());
        assert_eq!(handle.state(), crate::TaskState::Succeeded);
        assert_eq!(handle.exit_code(), Some(0));
        // The manager's task record was driven to the same terminal state and
        // can be cleaned up through the existing synchronous API.
        assert_eq!(manager.state("async"), Some(crate::TaskState::Succeeded));
        assert_eq!(manager.summaries()[0].state, crate::TaskState::Succeeded);
        assert_eq!(manager.clear_finished(), 1);
        assert_eq!(manager.state("async"), None);
    }

    #[test]
    fn spawn_async_timeout_kills_and_marks_timed_out() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("ping", vec!["127.0.0.1".into(), "-n".into(), "10".into()])
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
        let handle = manager
            .spawn_async_timeout(
                "slow",
                Path::new("."),
                std::time::Duration::from_millis(150),
                &mut events,
            )
            .unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("timed-out task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::TimedOut);
        assert_eq!(handle.state(), crate::TaskState::TimedOut);
        assert_eq!(manager.state("slow"), Some(crate::TaskState::TimedOut));
        // The async path observes completion through the handle, not events.
        assert!(events.drain("task.timeout").is_empty());
    }

    #[test]
    fn spawn_async_spawn_failure_finishes_task_synchronously() {
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
            .spawn_async("missing", Path::new("."), &mut events)
            .is_err());
        assert_eq!(manager.state("missing"), Some(crate::TaskState::Failed));
    }

    #[test]
    fn async_handle_cancel_kills_and_marks_cancelled() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            (
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Start-Sleep -Seconds 30".into(),
                ],
            )
        } else {
            ("sh", vec!["-c".into(), "exec sleep 30".into()])
        };
        manager
            .add(TaskSpec {
                id: "run".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("run", Path::new("."), &mut events)
            .unwrap();
        handle.cancel();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("cancelled task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::Cancelled);
        assert_eq!(manager.state("run"), Some(crate::TaskState::Cancelled));
    }

    /// A command that runs for 30s so a test can prove cancellation actually
    /// stopped the process: if the worker were not signalled, the process
    /// would outlive the short wait below and `wait_timeout` would time out.
    fn long_running_command() -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "powershell.exe".to_string(),
                vec![
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    "Start-Sleep -Seconds 30".to_string(),
                ],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "exec sleep 30".to_string()],
            )
        }
    }

    #[test]
    fn manager_cancel_stops_async_process_and_converges_state() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = long_running_command();
        manager
            .add(TaskSpec {
                id: "run".into(),
                program,
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("run", Path::new("."), &mut events)
            .unwrap();
        assert!(handle.is_running());
        assert_eq!(manager.state("run"), Some(crate::TaskState::Running));
        // Cancelling through the manager must signal the async worker: the
        // 30s background process is killed (otherwise the wait below would
        // time out while the handle stayed Running), and both the handle and
        // the manager record converge on Cancelled instead of diverging.
        manager.cancel("run", &mut events).unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("manager.cancel did not stop the async process");
        assert_eq!(snapshot.status, AsyncTaskStatus::Cancelled);
        assert_eq!(handle.state(), crate::TaskState::Cancelled);
        assert!(!handle.is_running());
        assert_eq!(manager.state("run"), Some(crate::TaskState::Cancelled));
        assert_eq!(manager.summaries()[0].state, crate::TaskState::Cancelled);
        assert_eq!(events.drain("task.cancelled")[0].payload, "run");
        // The finished record (and its cancel flag) is cleaned up by the
        // existing synchronous cleanup API.
        assert_eq!(manager.clear_finished(), 1);
        assert_eq!(manager.state("run"), None);
    }

    #[test]
    fn manager_cancel_keeps_working_for_sync_tasks() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "sync".into(),
                program: "sh".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("sync", &mut events).unwrap();
        manager.cancel("sync", &mut events).unwrap();
        assert_eq!(manager.state("sync"), Some(crate::TaskState::Cancelled));
        assert_eq!(events.drain("task.cancelled")[0].payload, "sync");
    }

    #[test]
    fn async_output_merges_stdout_and_stderr() {
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
                id: "mixed".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("mixed", Path::new("."), &mut events)
            .unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::Succeeded { exit_code: 0 });
        assert!(
            snapshot.output.contains("out"),
            "output was {:?}",
            snapshot.output
        );
        assert!(
            snapshot.output.contains("err"),
            "output was {:?}",
            snapshot.output
        );
    }

    #[test]
    fn async_completion_keeps_manager_record_output() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo hello".into()])
        } else {
            ("sh", vec!["-c".into(), "printf hello".into()])
        };
        manager
            .add(TaskSpec {
                id: "out".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("out", Path::new("."), &mut events)
            .unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::Succeeded { exit_code: 0 });
        assert!(
            snapshot.output.contains("hello"),
            "handle output was {:?}",
            snapshot.output
        );
        // The manager's task record must keep its own copy of the streamed
        // output: finish_async must not drain it into a discarded TaskResult.
        let task = manager.tasks.get("out").expect("record missing");
        assert_eq!(lock_task(task).state, crate::TaskState::Succeeded);
        assert!(
            lock_task(task).output().contains("hello"),
            "manager record lost its output after async completion: {:?}",
            lock_task(task).output()
        );
        // The finished record is still cleaned up by the sync API.
        assert_eq!(manager.clear_finished(), 1);
        assert_eq!(manager.state("out"), None);
    }

    #[test]
    fn async_timeout_keeps_manager_record_output() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec!["/C".into(), "echo hi & ping -n 10 127.0.0.1".into()],
            )
        } else {
            ("sh", vec!["-c".into(), "printf hi; sleep 30".into()])
        };
        manager
            .add(TaskSpec {
                id: "slow".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async_timeout(
                "slow",
                Path::new("."),
                std::time::Duration::from_millis(150),
                &mut events,
            )
            .unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("timed-out task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::TimedOut);
        assert!(
            snapshot.output.contains("hi"),
            "handle output was {:?}",
            snapshot.output
        );
        // TimedOut must retain the output in the manager record too, exactly
        // like Succeeded/Failed/Cancelled.
        let task = manager.tasks.get("slow").expect("record missing");
        assert_eq!(lock_task(task).state, crate::TaskState::TimedOut);
        assert!(
            lock_task(task).output().contains("hi"),
            "manager record lost its output after async timeout: {:?}",
            lock_task(task).output()
        );
    }

    #[test]
    fn async_worker_panic_never_leaves_handle_running() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "boom".into(),
                program: "sh".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("boom", &mut events).unwrap();
        let task = manager.tasks.get("boom").unwrap().clone();
        let handle = AsyncTaskHandle::new("boom");
        let shared = AsyncTaskShared {
            task,
            state: handle.state.clone(),
            done: handle.done.clone(),
            cancelled: handle.cancelled.clone(),
        };
        let thread = std::thread::spawn(move || {
            run_worker_safely(&shared, || panic!("simulated async worker panic"));
        });
        // The panic stays observable on the worker thread ...
        assert!(thread.join().is_err());
        // ... but the handle and the manager record converge on a terminal
        // state, so no waiter hangs and clear_finished can still clean up.
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(5))
            .expect("handle stayed Running forever after worker panic");
        assert_eq!(snapshot.status, AsyncTaskStatus::Failed { exit_code: -1 });
        assert!(!handle.is_running());
        assert_eq!(handle.state(), crate::TaskState::Failed);
        assert_eq!(manager.state("boom"), Some(crate::TaskState::Failed));
        assert_eq!(manager.clear_finished(), 1);
        assert_eq!(manager.state("boom"), None);
    }

    #[test]
    fn clear_finished_keeps_running_async_tasks() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = long_running_command();
        manager
            .add(TaskSpec {
                id: "run".into(),
                program,
                args,
            })
            .unwrap();
        let handle = manager
            .spawn_async("run", Path::new("."), &mut events)
            .unwrap();
        assert!(handle.is_running());
        // A task whose worker is still driving it must survive cleanup; only
        // terminal records are removed.
        assert_eq!(manager.clear_finished(), 0);
        assert_eq!(manager.state("run"), Some(crate::TaskState::Running));
        assert_eq!(manager.len(), 1);
        manager.cancel("run", &mut events).unwrap();
        let snapshot = handle
            .wait_timeout(std::time::Duration::from_secs(10))
            .expect("cancelled task never completed");
        assert_eq!(snapshot.status, AsyncTaskStatus::Cancelled);
        assert_eq!(manager.clear_finished(), 1);
        assert_eq!(manager.state("run"), None);
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

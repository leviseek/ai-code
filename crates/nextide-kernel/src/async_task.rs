//! A pollable handle to a task running in the background.
//!
//! [`AsyncTaskHandle`] is the async counterpart of the synchronous
//! [`crate::task_manager::TaskManager`] run methods. The UI (or any other
//! caller) starts a task with `TaskManager::spawn_async`, gets a handle back
//! immediately, keeps rendering or driving its own event loop, and polls the
//! handle for new output and completion status without ever blocking on the
//! underlying process.
//!
//! The handle is backed by shared, lock-protected state that the background
//! worker thread writes into as it streams the process: every poller observes
//! the same output and status. Lifecycle and output events — started, output,
//! finished, failed, cancelled, timed out — are additionally delivered through
//! a per-handle `std::sync::mpsc` channel, so a caller can poll the event
//! stream without ever moving the non-thread-safe [`crate::EventBus`] into the
//! worker thread. `wait`/`wait_timeout` block on a condvar that the worker
//! signals exactly once when the task reaches a terminal state, so a caller
//! can either poll in its own frame loop or park a thread until the task
//! finishes.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::task::TaskState;

/// Lifecycle status of a background task, observed through its handle.
///
/// Mirrors [`TaskState`] but carries the exit code for terminal
/// success/failure and lives in the handle's shared state, so pollers read it
/// without touching the manager's task record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsyncTaskStatus {
    /// The worker is still driving the process.
    Running,
    /// The process exited with code 0.
    Succeeded { exit_code: i32 },
    /// The process exited with a non-zero code (or the exit code is unknown).
    Failed { exit_code: i32 },
    /// The worker killed the process because the deadline elapsed.
    TimedOut,
    /// The worker killed the process because [`AsyncTaskHandle::cancel`] was
    /// called.
    Cancelled,
}

/// One event in a background task's lifecycle, delivered through the handle's
/// mpsc event channel.
///
/// The stream mirrors the synchronous `task.*` event topics so both views
/// speak the same language: `Started` ↔ `task.started`, `Output` ↔
/// `task.output`, `Finished` ↔ `task.finished`, `Failed` ↔ `task.failed`,
/// `TimedOut` ↔ `task.timeout`, `Cancelled` ↔ `task.cancelled`. Like the sync
/// bus, success ends with a single `Finished`, failure and timeout report
/// their cause first and then `Finished` (a timeout has no exit code, matching
/// the sync `task.finished` `{id}:timeout` payload), and cancellation emits
/// `Cancelled` alone.
///
/// Unlike the synchronous `task.output` events — which keep the lossy
/// per-chunk view of the raw bytes — `Output` carries the incrementally
/// decoded text, so concatenating every `Output` event reconstructs
/// [`AsyncTaskSnapshot::output`] exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsyncTaskEvent {
    /// The task was started (queued synchronously by
    /// [`AsyncTaskHandle::new`], always the first event).
    Started,
    /// A chunk of decoded output arrived.
    Output(String),
    /// The task reached a terminal state. `exit_code` is `None` for a timeout,
    /// mirroring the synchronous `task.finished` `{id}:timeout` payload.
    Finished { exit_code: Option<i32> },
    /// The process exited with a non-zero code (or the code is unknown).
    Failed { exit_code: i32 },
    /// The worker killed the process because the deadline elapsed.
    TimedOut,
    /// The worker killed the process because cancellation was requested.
    Cancelled,
}

/// A point-in-time view of a background task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsyncTaskSnapshot {
    pub id: String,
    pub status: AsyncTaskStatus,
    /// All output produced so far, decoded incrementally exactly as the
    /// synchronous streaming path decodes it (per-stream, so a multi-byte
    /// character split across chunks is reconstructed intact).
    pub output: String,
}

impl AsyncTaskSnapshot {
    /// The process exit code once the task reached `Succeeded` or `Failed`.
    pub fn exit_code(&self) -> Option<i32> {
        match &self.status {
            AsyncTaskStatus::Succeeded { exit_code } | AsyncTaskStatus::Failed { exit_code } => {
                Some(*exit_code)
            }
            _ => None,
        }
    }
}

/// Shared, lock-protected state behind an [`AsyncTaskHandle`].
///
/// `pub(crate)` because the background worker in `task_manager` writes into it
/// while pollers read through the handle.
#[derive(Debug)]
pub(crate) struct AsyncTaskState {
    pub(crate) status: AsyncTaskStatus,
    pub(crate) output: String,
    /// One mpsc sender per live [`AsyncTaskHandle`] clone. The background
    /// worker delivers every event to each sender, so every handle clone
    /// observes the same event stream independently of the others.
    event_senders: Vec<mpsc::Sender<AsyncTaskEvent>>,
}

impl AsyncTaskState {
    /// Delivers `event` to every live handle clone, in order. Senders whose
    /// receiver was dropped (the handle was dropped) are removed lazily, so a
    /// long-running task does not accumulate dead channels.
    pub(crate) fn publish(&mut self, event: AsyncTaskEvent) {
        let mut dead = Vec::new();
        for (index, sender) in self.event_senders.iter().enumerate() {
            if sender.send(event.clone()).is_err() {
                dead.push(index);
            }
        }
        for index in dead.into_iter().rev() {
            self.event_senders.swap_remove(index);
        }
    }
}

/// Handle to a task running in the background.
///
/// Clone the handle to share it: every clone observes the same underlying
/// task. Poll with [`AsyncTaskHandle::poll`] for a full snapshot or
/// [`AsyncTaskHandle::drain_output`] for only the output that arrived since
/// the previous call (each handle tracks its own read position). Lifecycle and
/// output arrive as a stream of [`AsyncTaskEvent`]s through
/// [`AsyncTaskHandle::poll_events`] / [`AsyncTaskHandle::drain_events`], each
/// clone receiving its own copy. Block with [`AsyncTaskHandle::wait`] /
/// [`AsyncTaskHandle::wait_timeout`], and request termination with
/// [`AsyncTaskHandle::cancel`].
///
/// All methods are non-blocking except `wait`/`wait_timeout`, which park the
/// calling thread until the worker signals completion. Dropping the handle
/// does **not** cancel the task: the worker keeps running until the process
/// exits, times out, or is cancelled, because the manager's task record and
/// any other cloned handle still reference it.
#[derive(Debug)]
pub struct AsyncTaskHandle {
    id: String,
    pub(crate) state: Arc<Mutex<AsyncTaskState>>,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) done: Arc<(Mutex<bool>, Condvar)>,
    /// Read position for [`AsyncTaskHandle::drain_output`]; each clone of the
    /// handle tracks its own position, so the method stays `&self` and the
    /// handle remains `Send + Sync`.
    delivered: AtomicUsize,
    /// This clone's event queue, fed by the worker through the shared sender
    /// registry in [`AsyncTaskState`]. `mpsc::Receiver` is not `Sync`, so it is
    /// wrapped in an `Arc<Mutex<..>>` to keep the handle `Send + Sync`; the
    /// lock is only ever held briefly inside the non-blocking poll methods.
    events: Arc<Mutex<mpsc::Receiver<AsyncTaskEvent>>>,
}

impl Clone for AsyncTaskHandle {
    fn clone(&self) -> Self {
        // Each clone gets its own event queue: register a fresh sender in the
        // shared registry so the worker delivers the same stream to it too.
        // Events published before the clone existed are not replayed, mirroring
        // how `drain_output` starts a cloned handle at the current position.
        let (sender, receiver) = mpsc::channel();
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .event_senders
            .push(sender);
        Self {
            id: self.id.clone(),
            state: Arc::clone(&self.state),
            cancelled: Arc::clone(&self.cancelled),
            done: Arc::clone(&self.done),
            delivered: AtomicUsize::new(self.delivered.load(Ordering::Relaxed)),
            events: Arc::new(Mutex::new(receiver)),
        }
    }
}

impl AsyncTaskHandle {
    /// Creates a handle for a task that is about to start running.
    ///
    /// The handle's event stream starts with [`AsyncTaskEvent::Started`],
    /// queued before any worker can publish, so a poller can rely on it being
    /// the first event.
    pub(crate) fn new(id: impl Into<String>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let handle = Self {
            id: id.into(),
            state: Arc::new(Mutex::new(AsyncTaskState {
                status: AsyncTaskStatus::Running,
                output: String::new(),
                event_senders: vec![sender],
            })),
            cancelled: Arc::new(AtomicBool::new(false)),
            done: Arc::new((Mutex::new(false), Condvar::new())),
            delivered: AtomicUsize::new(0),
            events: Arc::new(Mutex::new(receiver)),
        };
        handle
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .publish(AsyncTaskEvent::Started);
        handle
    }

    /// The task id this handle observes.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns whether the background worker is still driving the task.
    ///
    /// `false` means the task reached a terminal status (`Succeeded`,
    /// `Failed`, `TimedOut` or `Cancelled`).
    pub fn is_running(&self) -> bool {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.status == AsyncTaskStatus::Running
    }

    /// The task's lifecycle state, mapped from [`AsyncTaskStatus`] onto the
    /// manager's [`TaskState`] so UI code can treat both uniformly.
    pub fn state(&self) -> TaskState {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &guard.status {
            AsyncTaskStatus::Running => TaskState::Running,
            AsyncTaskStatus::Succeeded { .. } => TaskState::Succeeded,
            AsyncTaskStatus::Failed { .. } => TaskState::Failed,
            AsyncTaskStatus::TimedOut => TaskState::TimedOut,
            AsyncTaskStatus::Cancelled => TaskState::Cancelled,
        }
    }

    /// The process exit code once the task succeeded or failed.
    pub fn exit_code(&self) -> Option<i32> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &guard.status {
            AsyncTaskStatus::Succeeded { exit_code } | AsyncTaskStatus::Failed { exit_code } => {
                Some(*exit_code)
            }
            _ => None,
        }
    }

    /// A full snapshot of the task: current status plus every byte of output
    /// produced so far. Never blocks.
    ///
    /// For per-frame UI updates prefer [`AsyncTaskHandle::drain_output`],
    /// which avoids re-reading the whole history.
    pub fn poll(&self) -> AsyncTaskSnapshot {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        AsyncTaskSnapshot {
            id: self.id.clone(),
            status: guard.status.clone(),
            output: guard.output.clone(),
        }
    }

    /// Returns only the output that arrived since this handle's previous
    /// `drain_output` call, and advances the read position. Never blocks.
    ///
    /// Each clone of the handle tracks its own read position, starting at the
    /// position the cloned handle had reached, so a clone never replays output
    /// that was delivered before it existed.
    pub fn drain_output(&self) -> String {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let delivered = self.delivered.load(Ordering::Relaxed);
        if guard.output.len() > delivered {
            let text = guard.output[delivered..].to_string();
            self.delivered.store(guard.output.len(), Ordering::Relaxed);
            text
        } else {
            String::new()
        }
    }

    /// Returns the next pending event, if any. Never blocks.
    ///
    /// Each event is consumed exactly once by whichever clone polls its own
    /// queue. For a per-frame loop that wants everything since the last frame,
    /// prefer [`AsyncTaskHandle::drain_events`].
    pub fn poll_events(&self) -> Option<AsyncTaskEvent> {
        let receiver = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receiver.try_recv().ok()
    }

    /// Returns every event queued since the previous call, in arrival order,
    /// and empties the queue. Never blocks.
    ///
    /// The stream always starts with [`AsyncTaskEvent::Started`] (queued by
    /// [`AsyncTaskHandle::new`]) and ends with a terminal event — except for
    /// clones, which observe only events published after the clone was
    /// created. Concatenating the [`AsyncTaskEvent::Output`] payloads
    /// reconstructs [`AsyncTaskSnapshot::output`] exactly.
    pub fn drain_events(&self) -> Vec<AsyncTaskEvent> {
        let receiver = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    /// Blocks until the task reaches a terminal status, then returns a full
    /// snapshot. Use [`AsyncTaskHandle::wait_timeout`] when a deadline must be
    /// enforced.
    pub fn wait(&self) -> AsyncTaskSnapshot {
        let (lock, cvar) = &*self.done;
        let mut finished = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*finished {
            finished = cvar
                .wait(finished)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        drop(finished);
        self.poll()
    }

    /// Blocks until the task reaches a terminal status or `timeout` elapses.
    ///
    /// Returns `None` if the timeout elapsed first and the task is still
    /// running. The budget is measured from the call, not per wakeup: a
    /// spurious condvar notification (or any notify without completion) cannot
    /// restart the full timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<AsyncTaskSnapshot> {
        let (lock, cvar) = &*self.done;
        let mut finished = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = std::time::Instant::now() + timeout;
        while !*finished {
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (guard, result) =
                cvar.wait_timeout(finished, remaining)
                    .unwrap_or_else(|poisoned| {
                        let (guard, result) = poisoned.into_inner();
                        (guard, result)
                    });
            finished = guard;
            if result.timed_out() && !*finished {
                return None;
            }
        }
        drop(finished);
        Some(self.poll())
    }

    /// Requests termination of the task: the worker kills the process (and its
    /// descendants) and the status becomes [`AsyncTaskStatus::Cancelled`] once
    /// it has been reaped. Never blocks.
    ///
    /// Calling this on a task that already reached a terminal status is a
    /// no-op.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_polls_shared_state_and_waits_for_done() {
        let handle = AsyncTaskHandle::new("t");
        assert!(handle.is_running());
        assert_eq!(handle.state(), TaskState::Running);
        assert_eq!(handle.exit_code(), None);

        // Simulate the background worker publishing output.
        handle.state.lock().unwrap().output.push_str("hello");
        assert_eq!(handle.poll().output, "hello");
        assert_eq!(handle.drain_output(), "hello");
        // Incremental: a second drain returns nothing new.
        assert_eq!(handle.drain_output(), "");

        // Simulate the worker finishing the task and signaling completion.
        {
            let mut state = handle.state.lock().unwrap();
            state.status = AsyncTaskStatus::Succeeded { exit_code: 0 };
            let (lock, cvar) = &*handle.done;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }

        let snapshot = handle
            .wait_timeout(Duration::from_secs(5))
            .expect("wait_timeout never observed completion");
        assert_eq!(snapshot.id, "t");
        assert_eq!(snapshot.status, AsyncTaskStatus::Succeeded { exit_code: 0 });
        assert_eq!(snapshot.exit_code(), Some(0));
        assert!(!handle.is_running());
        assert_eq!(handle.state(), TaskState::Succeeded);

        // Cancelling a finished task is a no-op.
        handle.cancel();
        assert_eq!(handle.state(), TaskState::Succeeded);
    }

    #[test]
    fn handle_wait_blocks_until_notified() {
        let handle = AsyncTaskHandle::new("t");
        let clone = handle.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut state = clone.state.lock().unwrap();
            state.status = AsyncTaskStatus::Failed { exit_code: 1 };
            let (lock, cvar) = &*clone.done;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
        let snapshot = handle.wait();
        worker.join().unwrap();
        assert_eq!(snapshot.status, AsyncTaskStatus::Failed { exit_code: 1 });
        assert_eq!(handle.exit_code(), Some(1));
        assert_eq!(handle.state(), TaskState::Failed);
    }

    #[test]
    fn handle_event_channel_starts_with_started_then_streams_events() {
        let handle = AsyncTaskHandle::new("t");
        // `new` queues Started synchronously, before any worker can publish.
        assert_eq!(handle.poll_events(), Some(AsyncTaskEvent::Started));
        assert_eq!(handle.poll_events(), None);

        // Simulate the worker streaming decoded output.
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("hello");
            state.publish(AsyncTaskEvent::Output("hello".to_owned()));
        }
        assert_eq!(
            handle.drain_events(),
            vec![AsyncTaskEvent::Output("hello".to_owned())]
        );
        assert!(handle.drain_events().is_empty());

        // Simulate the worker finishing successfully.
        {
            let mut state = handle.state.lock().unwrap();
            state.status = AsyncTaskStatus::Succeeded { exit_code: 0 };
            state.publish(AsyncTaskEvent::Finished { exit_code: Some(0) });
            let (lock, cvar) = &*handle.done;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        assert_eq!(
            handle.drain_events(),
            vec![AsyncTaskEvent::Finished { exit_code: Some(0) }]
        );
    }

    #[test]
    fn handle_event_channel_mirrors_failed_and_timeout_terminals() {
        let handle = AsyncTaskHandle::new("t");
        assert_eq!(handle.drain_events(), vec![AsyncTaskEvent::Started]);
        {
            let mut state = handle.state.lock().unwrap();
            state.status = AsyncTaskStatus::Failed { exit_code: 3 };
            state.publish(AsyncTaskEvent::Failed { exit_code: 3 });
            state.publish(AsyncTaskEvent::Finished { exit_code: Some(3) });
            let (lock, cvar) = &*handle.done;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        assert_eq!(
            handle.drain_events(),
            vec![
                AsyncTaskEvent::Failed { exit_code: 3 },
                AsyncTaskEvent::Finished { exit_code: Some(3) },
            ]
        );

        let timed_out = AsyncTaskHandle::new("t2");
        assert_eq!(timed_out.drain_events(), vec![AsyncTaskEvent::Started]);
        {
            let mut state = timed_out.state.lock().unwrap();
            state.status = AsyncTaskStatus::TimedOut;
            state.publish(AsyncTaskEvent::TimedOut);
            state.publish(AsyncTaskEvent::Finished { exit_code: None });
            let (lock, cvar) = &*timed_out.done;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        assert_eq!(
            timed_out.drain_events(),
            vec![
                AsyncTaskEvent::TimedOut,
                AsyncTaskEvent::Finished { exit_code: None },
            ]
        );
    }

    #[test]
    fn handle_clones_each_receive_the_whole_event_stream() {
        let handle = AsyncTaskHandle::new("t");
        let clone = handle.clone();
        // The clone registered its sender after Started was published, so it
        // observes only events that arrive from now on.
        assert_eq!(handle.drain_events(), vec![AsyncTaskEvent::Started]);
        assert!(clone.drain_events().is_empty());

        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("a");
            state.publish(AsyncTaskEvent::Output("a".to_owned()));
            state.output.push_str("b");
            state.publish(AsyncTaskEvent::Output("b".to_owned()));
        }
        // Both clones receive every event, independently and in order.
        assert_eq!(
            handle.drain_events(),
            vec![
                AsyncTaskEvent::Output("a".to_owned()),
                AsyncTaskEvent::Output("b".to_owned()),
            ]
        );
        assert_eq!(
            clone.drain_events(),
            vec![
                AsyncTaskEvent::Output("a".to_owned()),
                AsyncTaskEvent::Output("b".to_owned()),
            ]
        );
    }

    #[test]
    fn clone_created_mid_stream_sees_only_future_events() {
        let handle = AsyncTaskHandle::new("t");
        assert_eq!(handle.drain_events(), vec![AsyncTaskEvent::Started]);
        // Publish one event before the clone exists.
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("a");
            state.publish(AsyncTaskEvent::Output("a".to_owned()));
        }
        let clone = handle.clone();
        // The clone registered after "a" was published: it must not replay it.
        assert!(clone.drain_events().is_empty());
        // ... but it observes everything published from now on, exactly once,
        // independently of the original handle's queue.
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("b");
            state.publish(AsyncTaskEvent::Output("b".to_owned()));
            state.publish(AsyncTaskEvent::Output("c".to_owned()));
        }
        assert_eq!(
            handle.drain_events(),
            vec![
                AsyncTaskEvent::Output("a".to_owned()),
                AsyncTaskEvent::Output("b".to_owned()),
                AsyncTaskEvent::Output("c".to_owned()),
            ]
        );
        assert_eq!(
            clone.drain_events(),
            vec![
                AsyncTaskEvent::Output("b".to_owned()),
                AsyncTaskEvent::Output("c".to_owned()),
            ]
        );
    }

    #[test]
    fn dropped_clone_senders_are_cleaned_up() {
        let handle = AsyncTaskHandle::new("t");
        assert_eq!(handle.drain_events(), vec![AsyncTaskEvent::Started]);
        let clone = handle.clone();
        {
            let state = handle.state.lock().unwrap();
            assert_eq!(state.event_senders.len(), 2);
        }
        drop(clone);
        // The next publish notices the dropped receiver and removes its sender,
        // so a long-running task does not accumulate dead channels.
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("x");
            state.publish(AsyncTaskEvent::Output("x".to_owned()));
            assert_eq!(state.event_senders.len(), 1);
        }
        assert_eq!(
            handle.drain_events(),
            vec![AsyncTaskEvent::Output("x".to_owned())]
        );
    }

    #[test]
    fn clone_drain_output_starts_at_the_clone_point() {
        let handle = AsyncTaskHandle::new("t");
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str("hello");
        }
        assert_eq!(handle.drain_output(), "hello");
        let clone = handle.clone();
        // The clone inherits the parent's read position instead of starting
        // from the beginning of the stream.
        {
            let mut state = handle.state.lock().unwrap();
            state.output.push_str(" world");
        }
        assert_eq!(clone.drain_output(), " world");
        assert_eq!(handle.drain_output(), " world");
        assert!(clone.drain_output().is_empty());
    }

    #[test]
    fn handle_wait_timeout_respects_total_budget_under_spurious_wakeups() {
        let handle = AsyncTaskHandle::new("t");
        // A helper thread keeps notifying the condvar without ever finishing
        // the task. Each wakeup would restart a full timeout if the remaining
        // budget were not recomputed, so wait_timeout must still return None
        // once the caller's total budget elapses.
        let done = Arc::clone(&handle.done);
        let stop = Arc::new(AtomicBool::new(false));
        let notifier_stop = Arc::clone(&stop);
        let notifier = std::thread::spawn(move || {
            let (lock, cvar) = &*done;
            let started = std::time::Instant::now();
            while !notifier_stop.load(Ordering::Relaxed)
                && started.elapsed() < Duration::from_millis(500)
            {
                {
                    let _guard = lock.lock().unwrap();
                    cvar.notify_all();
                }
                // Release the lock before sleeping: if the notifier kept
                // holding it, the waiter could never reacquire it between
                // wakeups and the test would measure lock starvation instead
                // of the timeout budget.
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let started = std::time::Instant::now();
        let result = handle.wait_timeout(Duration::from_millis(100));
        stop.store(true, Ordering::Relaxed);
        notifier.join().unwrap();
        assert!(result.is_none(), "a still-running task must time out");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "wait_timeout overran its total budget: {:?}",
            started.elapsed()
        );
    }
}

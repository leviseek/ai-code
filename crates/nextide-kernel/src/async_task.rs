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
//! the same output and status. `wait`/`wait_timeout` block on a condvar that
//! the worker signals exactly once when the task reaches a terminal state, so
//! a caller can either poll in its own frame loop or park a thread until the
//! task finishes.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
}

/// Handle to a task running in the background.
///
/// Clone the handle to share it: every clone observes the same underlying
/// task. Poll with [`AsyncTaskHandle::poll`] for a full snapshot or
/// [`AsyncTaskHandle::drain_output`] for only the output that arrived since
/// the previous call (each handle tracks its own read position). Block with
/// [`AsyncTaskHandle::wait`] / [`AsyncTaskHandle::wait_timeout`], and request
/// termination with [`AsyncTaskHandle::cancel`].
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
}

impl Clone for AsyncTaskHandle {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            state: Arc::clone(&self.state),
            cancelled: Arc::clone(&self.cancelled),
            done: Arc::clone(&self.done),
            delivered: AtomicUsize::new(self.delivered.load(Ordering::Relaxed)),
        }
    }
}

impl AsyncTaskHandle {
    /// Creates a handle for a task that is about to start running.
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            state: Arc::new(Mutex::new(AsyncTaskState {
                status: AsyncTaskStatus::Running,
                output: String::new(),
            })),
            cancelled: Arc::new(AtomicBool::new(false)),
            done: Arc::new((Mutex::new(false), Condvar::new())),
            delivered: AtomicUsize::new(0),
        }
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
    /// Each clone of the handle tracks its own read position, so a cloned
    /// handle starts from the beginning again.
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
    /// running.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<AsyncTaskSnapshot> {
        let (lock, cvar) = &*self.done;
        let mut finished = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*finished {
            let (guard, result) = cvar
                .wait_timeout(finished, timeout)
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
}

//! A thread-safe bridge that lets a background task worker hand its lifecycle
//! and output events to the caller without ever holding the non-thread-safe
//! [`EventBus`].
//!
//! [`EventBus`] is owned and mutated by the main (UI) thread, but
//! [`crate::TaskManager::spawn_async_with_sink`] drives its worker on a
//! background thread that cannot take `&mut EventBus`.
//! [`AsyncTaskEventSink`] is the producer half of the bridge — a cloneable,
//! `Send + Sync` handle the worker pushes [`AsyncTaskEvent`]s into — and
//! [`TaskEventReceiver`] is the consumer half. The UI thread polls the
//! receiver each frame ([`TaskEventReceiver::poll`] /
//! [`TaskEventReceiver::drain`]) and replays what it consumed into its
//! [`EventBus`] with [`TaskEventReceiver::publish_into`], so the async task
//! stream and the synchronous `task.*` topics stay unified without sharing any
//! mutable bus across threads.
//!
//! The bridged event stream mirrors the one delivered by
//! [`crate::AsyncTaskHandle::drain_events`]: it starts with
//! [`AsyncTaskEvent::Started`], streams [`AsyncTaskEvent::Output`] events as
//! decoded output arrives, and ends with the task's terminal event —
//! [`AsyncTaskEvent::Finished`] (preceded by
//! [`AsyncTaskEvent::Failed`]/[`AsyncTaskEvent::TimedOut`] when applicable) or
//! [`AsyncTaskEvent::Cancelled`] alone. Unlike the handle's per-clone queues,
//! this is a single-consumer stream: the queue is unbounded and mutex-guarded,
//! so any number of producer threads may push, but consumers share the one
//! queue (concurrent consumers would partition the events between them).
//!
//! # Example
//!
//! ```
//! use nextide_kernel::{AsyncTaskEventSink, EventBus, TaskManager, TaskSpec};
//! use std::path::Path;
//!
//! let mut manager = TaskManager::default();
//! let mut bus = EventBus::default();
//! # let (program, args) = if cfg!(windows) {
//! #     ("cmd.exe", vec!["/C".into(), "echo via-bridge".into()])
//! # } else {
//! #     ("sh", vec!["-c".into(), "printf via-bridge".into()])
//! # };
//! # manager.add(TaskSpec {
//! #     id: "build".into(),
//! #     program: program.into(),
//! #     args,
//! # }).unwrap();
//!
//! // Hand the sink to the background worker; the worker never sees the
//! // EventBus, which stays on the main thread.
//! let (sink, receiver) = AsyncTaskEventSink::channel("build");
//! let _handle = manager.spawn_async_with_sink(
//!     "build",
//!     Path::new("."),
//!     &mut bus,
//!     sink,
//! )?;
//!
//! // Each frame, on the main thread, consume the bridge and replay the
//! // worker's lifecycle/output events into the EventBus.
//! let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
//! while !receiver.is_closed() {
//!     receiver.publish_into(&mut bus);
//!     assert!(std::time::Instant::now() < deadline, "bridge never closed");
//!     std::thread::sleep(std::time::Duration::from_millis(10));
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::async_task::AsyncTaskEvent;
use crate::{Event, EventBus};

/// Lock-protected queue shared by a sink/receiver pair.
#[derive(Debug)]
struct TaskEventShared {
    queue: Mutex<VecDeque<AsyncTaskEvent>>,
    /// Wakes blocking waiters when an event is pushed or the stream ends.
    waker: Condvar,
    /// Number of live [`AsyncTaskEventSink`] clones, including the original.
    /// When it reaches zero, no further events can be pushed.
    producers: AtomicUsize,
    /// Set by [`AsyncTaskEventSink::close`] to end the stream while sink
    /// clones may still be alive.
    closed: AtomicBool,
}

impl TaskEventShared {
    fn push(&self, event: AsyncTaskEvent) {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(event);
        self.waker.notify_one();
    }

    /// Whether the stream is over: explicitly closed or every producer is
    /// gone, **and** nothing is buffered anymore. `queue_empty` is passed in
    /// because callers usually already hold the queue lock.
    fn ended(&self, queue_empty: bool) -> bool {
        let finished =
            self.closed.load(Ordering::Acquire) || self.producers.load(Ordering::Acquire) == 0;
        finished && queue_empty
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.waker.notify_all();
    }
}

/// Producer half of a task-event bridge.
///
/// Clone the sink to share it: every clone pushes into the same stream, so any
/// number of producer threads can contribute (the queue is mutex-guarded and
/// unbounded). Drop the last clone (or call [`AsyncTaskEventSink::close`]) to
/// end the stream; a blocked [`TaskEventReceiver::wait`] then wakes and
/// returns `None` once the queue is drained.
///
/// The sink itself never touches an [`EventBus`] — it only queues
/// [`AsyncTaskEvent`]s. Replaying them into the bus is the receiver's job, on
/// the main thread.
#[derive(Debug)]
pub struct AsyncTaskEventSink {
    shared: Arc<TaskEventShared>,
}

impl AsyncTaskEventSink {
    /// Creates a new sink/receiver pair bridged to the task with the given
    /// `id`. The id is carried by the receiver for
    /// [`TaskEventReceiver::publish_into`], which replays the stream as
    /// `task.*` events payloaded with this id.
    pub fn channel(id: impl Into<String>) -> (Self, TaskEventReceiver) {
        let shared = Arc::new(TaskEventShared {
            queue: Mutex::new(VecDeque::new()),
            waker: Condvar::new(),
            producers: AtomicUsize::new(1),
            closed: AtomicBool::new(false),
        });
        let receiver = TaskEventReceiver {
            shared: Arc::clone(&shared),
            id: id.into(),
        };
        (Self { shared }, receiver)
    }

    /// Appends one event to the stream. Never blocks; the queue is unbounded,
    /// so a caller that polls rarely should drain often.
    pub fn push(&self, event: AsyncTaskEvent) {
        self.shared.push(event);
    }

    /// Signals that no further events will be pushed, waking any blocked
    /// [`TaskEventReceiver::wait`] so it can observe the end of the stream.
    ///
    /// Producers should stop pushing after `close`; events pushed anyway are
    /// still delivered, but the stream stays marked closed. This is a no-op
    /// once the stream is already closed.
    pub fn close(&self) {
        self.shared.close();
    }
}

impl Clone for AsyncTaskEventSink {
    fn clone(&self) -> Self {
        self.shared.producers.fetch_add(1, Ordering::AcqRel);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for AsyncTaskEventSink {
    fn drop(&mut self) {
        if self.shared.producers.fetch_sub(1, Ordering::AcqRel) == 1 {
            // The last producer is gone: wake any blocked waiter so it can
            // observe the closed stream instead of sleeping forever.
            self.shared.waker.notify_all();
        }
    }
}

/// Consumer half of a task-event bridge.
///
/// Poll it from the UI frame loop ([`TaskEventReceiver::poll`] /
/// [`TaskEventReceiver::drain`]) or block on it
/// ([`TaskEventReceiver::wait`] / [`TaskEventReceiver::wait_timeout`]), then
/// replay what was consumed into a caller-owned [`EventBus`] on the main
/// thread with [`TaskEventReceiver::publish_into`]. The receiver is intended
/// for a single consumer; the queue operations are mutex-guarded, so
/// concurrent access is memory-safe but would partition events between
/// consumers.
#[derive(Debug)]
pub struct TaskEventReceiver {
    shared: Arc<TaskEventShared>,
    id: String,
}

impl TaskEventReceiver {
    /// The task id this bridge was created for (used by
    /// [`TaskEventReceiver::publish_into`] to payload the `task.*` events).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The number of buffered, not-yet-consumed events. Never blocks.
    pub fn pending(&self) -> usize {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Returns the next pending event, if any. Never blocks.
    pub fn poll(&self) -> Option<AsyncTaskEvent> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }

    /// Returns every event queued since the previous call, in arrival order,
    /// and empties the queue. Never blocks.
    pub fn drain(&self) -> Vec<AsyncTaskEvent> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.drain(..).collect()
    }

    /// Blocks until the next event is available or the stream ends (every
    /// sink dropped or [`AsyncTaskEventSink::close`] called) and the queue is
    /// drained. Returns the event, or `None` once the ended stream has been
    /// fully consumed.
    pub fn wait(&self) -> Option<AsyncTaskEvent> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(event) = queue.pop_front() {
                return Some(event);
            }
            if self.shared.ended(queue.is_empty()) {
                return None;
            }
            queue = self
                .shared
                .waker
                .wait(queue)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Blocks until the next event is available or `timeout` elapses.
    ///
    /// Returns `None` if nothing arrived before the timeout — including when
    /// the stream ended with an empty queue; use
    /// [`TaskEventReceiver::is_closed`] to tell the two apart. The budget is
    /// measured from the call, not per wakeup, so a spurious condvar
    /// notification cannot extend it.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<AsyncTaskEvent> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = queue.pop_front() {
                return Some(event);
            }
            if self.shared.ended(queue.is_empty()) {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (guard, result) = self
                .shared
                .waker
                .wait_timeout(queue, remaining)
                .unwrap_or_else(|poisoned| {
                    let (guard, result) = poisoned.into_inner();
                    (guard, result)
                });
            queue = guard;
            if result.timed_out() {
                // A push may have raced with the timeout: consume it if so.
                return queue.pop_front();
            }
        }
    }

    /// Blocks up to `timeout`, consuming events, and returns the first
    /// terminal event ([`AsyncTaskEvent::Finished`] or
    /// [`AsyncTaskEvent::Cancelled`]).
    ///
    /// Intermediate events are consumed and discarded, so this is meant for
    /// callers that only care about the final outcome (e.g. tests); a UI that
    /// needs the output should poll with
    /// [`TaskEventReceiver::poll`]/[`TaskEventReceiver::drain`] instead.
    /// Returns `None` if no terminal event arrived in time.
    pub fn wait_terminal_timeout(&self, timeout: Duration) -> Option<AsyncTaskEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.wait_timeout(remaining) {
                Some(event @ (AsyncTaskEvent::Finished { .. } | AsyncTaskEvent::Cancelled)) => {
                    return Some(event);
                }
                Some(_) => continue,
                None => return None,
            }
        }
    }

    /// Consumes every buffered event and replays it into `bus` as a `task.*`
    /// event on the calling thread, returning how many events were replayed.
    ///
    /// The mapping mirrors the synchronous `EventBus` topics, payloaded with
    /// this receiver's task id:
    ///
    /// | [`AsyncTaskEvent`] | topic | payload |
    /// |---|---|---|
    /// | `Started` | `task.started` | `{id}` |
    /// | `Output(text)` | `task.output` | `{id}:{text}` |
    /// | `Failed { code }` | `task.failed` | `{id}:{code}` |
    /// | `Finished { Some(code) }` | `task.finished` | `{id}:{code}` |
    /// | `Finished { None }` | `task.finished` | `{id}:timeout` |
    /// | `TimedOut` | `task.timeout` | `{id}` |
    /// | `Cancelled` | `task.cancelled` | `{id}` |
    ///
    /// Unlike the synchronous streaming path — whose `task.output` events keep
    /// the lossy per-chunk byte view — the bridged `Output` events carry the
    /// incrementally decoded text, exactly as [`crate::AsyncTaskHandle`]
    /// delivers it. The whole stream is replayed, including `Started`: the
    /// synchronous spawn path also publishes `task.started` up front, so a bus
    /// fed by both observes the started event twice.
    ///
    /// Call this on the main thread only: [`EventBus`] is not thread-safe.
    pub fn publish_into(&self, bus: &mut EventBus) -> usize {
        let events = self.drain();
        let count = events.len();
        for event in events {
            let (topic, payload) = match event {
                AsyncTaskEvent::Started => ("task.started", self.id.clone()),
                AsyncTaskEvent::Output(text) => ("task.output", format!("{}:{text}", self.id)),
                AsyncTaskEvent::Failed { exit_code } => {
                    ("task.failed", format!("{}:{exit_code}", self.id))
                }
                AsyncTaskEvent::Finished {
                    exit_code: Some(code),
                } => ("task.finished", format!("{}:{code}", self.id)),
                AsyncTaskEvent::Finished { exit_code: None } => {
                    ("task.finished", format!("{}:timeout", self.id))
                }
                AsyncTaskEvent::TimedOut => ("task.timeout", self.id.clone()),
                AsyncTaskEvent::Cancelled => ("task.cancelled", self.id.clone()),
            };
            bus.publish(Event::new(topic, payload));
        }
        count
    }

    /// Whether the stream has ended — every sink dropped or
    /// [`AsyncTaskEventSink::close`] called — **and** every buffered event has
    /// been consumed, i.e. no event will ever arrive again.
    ///
    /// A UI pump can loop `while !receiver.is_closed() {
    /// receiver.publish_into(&mut bus); /* sleep to the next frame */ }`.
    pub fn is_closed(&self) -> bool {
        let queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.shared.ended(queue.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_task::AsyncTaskEvent;

    #[test]
    fn channel_delivers_events_in_fifo_order() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        assert_eq!(receiver.id(), "t");
        assert_eq!(receiver.pending(), 0);
        assert!(receiver.poll().is_none());
        assert!(receiver.drain().is_empty());

        sink.push(AsyncTaskEvent::Started);
        sink.push(AsyncTaskEvent::Output("hello".to_owned()));
        assert_eq!(receiver.pending(), 2);
        assert_eq!(receiver.poll(), Some(AsyncTaskEvent::Started));
        assert_eq!(receiver.pending(), 1);
        sink.push(AsyncTaskEvent::Finished { exit_code: Some(0) });
        assert_eq!(
            receiver.drain(),
            vec![
                AsyncTaskEvent::Output("hello".to_owned()),
                AsyncTaskEvent::Finished { exit_code: Some(0) },
            ]
        );
        assert_eq!(receiver.pending(), 0);
    }

    #[test]
    fn receiver_waits_for_events_with_and_without_timeout() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        // wait_timeout on a live, empty channel returns None once the timeout
        // elapses.
        assert_eq!(receiver.wait_timeout(Duration::from_millis(20)), None);

        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            sink.push(AsyncTaskEvent::Output("later".to_owned()));
            sink.close();
        });
        assert_eq!(
            receiver.wait_timeout(Duration::from_secs(5)),
            Some(AsyncTaskEvent::Output("later".to_owned()))
        );
        // The producer closed the stream: wait() returns None instead of
        // blocking forever, and the receiver reports the end.
        assert_eq!(receiver.wait(), None);
        assert!(receiver.is_closed());
        producer.join().unwrap();
    }

    #[test]
    fn close_wakes_blocked_waiters() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        let parked = std::thread::spawn(move || receiver.wait());
        std::thread::sleep(Duration::from_millis(50));
        sink.close();
        assert_eq!(parked.join().unwrap(), None);
    }

    #[test]
    fn concurrent_producers_deliver_every_event_without_loss() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        const PRODUCERS: usize = 4;
        const EVENTS_PER_PRODUCER: usize = 100;
        let mut threads = Vec::new();
        for index in 0..PRODUCERS {
            let sink = sink.clone();
            threads.push(std::thread::spawn(move || {
                for item in 0..EVENTS_PER_PRODUCER {
                    sink.push(AsyncTaskEvent::Output(format!("{index}-{item}")));
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        // Dropping the original sink leaves no producers: the stream closes
        // once the queue is drained.
        drop(sink);
        let mut received = 0;
        while let Some(event) = receiver.wait() {
            match event {
                AsyncTaskEvent::Output(_) => received += 1,
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(received, PRODUCERS * EVENTS_PER_PRODUCER);
        assert!(receiver.is_closed());
    }

    #[test]
    fn dropping_all_sinks_closes_the_stream() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        let clone = sink.clone();
        drop(clone);
        // The original sink is still alive: the stream is not closed yet.
        assert!(!receiver.is_closed());
        drop(sink);
        assert!(receiver.is_closed());
        assert_eq!(receiver.wait(), None);
    }

    #[test]
    fn is_closed_stays_false_until_buffered_events_are_consumed() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        sink.push(AsyncTaskEvent::Started);
        sink.close();
        // The stream ended, but the buffered event must still be consumed.
        assert!(!receiver.is_closed());
        assert_eq!(receiver.poll(), Some(AsyncTaskEvent::Started));
        assert!(receiver.is_closed());
    }

    #[test]
    fn publish_into_replays_the_stream_as_sync_task_topics() {
        let (sink, receiver) = AsyncTaskEventSink::channel("build");
        sink.push(AsyncTaskEvent::Started);
        sink.push(AsyncTaskEvent::Output("compiling".to_owned()));
        sink.push(AsyncTaskEvent::Failed { exit_code: 3 });
        sink.push(AsyncTaskEvent::Finished { exit_code: Some(3) });
        let mut bus = EventBus::default();
        assert_eq!(receiver.publish_into(&mut bus), 4);
        assert_eq!(bus.drain("task.started")[0].payload, "build");
        assert_eq!(bus.drain("task.output")[0].payload, "build:compiling");
        assert_eq!(bus.drain("task.failed")[0].payload, "build:3");
        assert_eq!(bus.drain("task.finished")[0].payload, "build:3");
        assert!(bus.drain("task.timeout").is_empty());
        assert!(bus.drain("task.cancelled").is_empty());
    }

    #[test]
    fn publish_into_maps_timeout_and_cancelled_topics() {
        let (sink, receiver) = AsyncTaskEventSink::channel("slow");
        sink.push(AsyncTaskEvent::Started);
        sink.push(AsyncTaskEvent::TimedOut);
        sink.push(AsyncTaskEvent::Finished { exit_code: None });
        let mut bus = EventBus::default();
        receiver.publish_into(&mut bus);
        assert_eq!(bus.drain("task.timeout")[0].payload, "slow");
        assert_eq!(bus.drain("task.finished")[0].payload, "slow:timeout");

        let (sink, receiver) = AsyncTaskEventSink::channel("run");
        sink.push(AsyncTaskEvent::Started);
        sink.push(AsyncTaskEvent::Cancelled);
        let mut bus = EventBus::default();
        receiver.publish_into(&mut bus);
        assert_eq!(bus.drain("task.cancelled")[0].payload, "run");
        assert!(bus.drain("task.finished").is_empty());
    }

    #[test]
    fn wait_terminal_timeout_skips_intermediate_events() {
        let (sink, receiver) = AsyncTaskEventSink::channel("t");
        let producer = std::thread::spawn(move || {
            for line in ["one", "two"] {
                sink.push(AsyncTaskEvent::Output(line.to_owned()));
                std::thread::sleep(Duration::from_millis(20));
            }
            sink.push(AsyncTaskEvent::Finished { exit_code: Some(0) });
        });
        assert_eq!(
            receiver.wait_terminal_timeout(Duration::from_secs(5)),
            Some(AsyncTaskEvent::Finished { exit_code: Some(0) })
        );
        producer.join().unwrap();
    }
}

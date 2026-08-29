//! A tiny, dependency-free event hub with a legacy per-topic poll/drain API,
//! per-subscriber subscriptions, and a bounded in-memory event log.
//!
//! [`EventBus::publish`] appends an [`Event`] to the topic's pending queue —
//! the legacy [`EventBus::pending`] / [`EventBus::drain`] API — and stamps it
//! with a bus-wide, monotonically increasing sequence number ([`Event::seq`]).
//! Sequence numbers are assigned at publish time and never reused, so every
//! event the bus has published is totally ordered across topics.
//!
//! [`EventBus::subscribe`] creates an [`EventSubscriber`]: a per-subscriber
//! subscription with its own independent queue and a watermark that starts at
//! the next sequence number at subscription time. Delivery is pull-based —
//! `subscriber.drain(&bus)` copies the bus's currently pending events for the
//! subscribed topic that were published since the previous pull into the
//! subscriber's queue and returns them in publish order, and
//! `subscriber.pending(&bus)` reports how many a next `drain` would deliver.
//! A pull never modifies the bus, so legacy `drain` consumers and every other
//! subscriber observe exactly the same events: each subscriber receives the
//! full stream independently of the others.
//!
//! [`EventBus::log`] and [`EventBus::events_since`] expose the bus's retained
//! event log: a per-topic, bounded in-memory journal that every published
//! event is appended to and that *no* consumer path ever removes from. Both
//! return copies of events — [`EventBus::log`] the whole retained log for a
//! topic, [`EventBus::events_since`] the retained events with `seq >= cursor`
//! — without consuming the pending queue, so legacy `drain`, subscribers, and
//! the log each observe the same events independently. Each topic's log holds
//! at most [`EventBus::log_capacity`] events ([`EventBus::with_log_capacity`]
//! configures it; the default is finite); when a publish would exceed the
//! capacity the *oldest* retained events are dropped first, and
//! [`EventBus::log_start`] reports the oldest retained sequence number so a
//! cursor reader can detect that eviction happened.
//!
//! A subscriber is a plain owned value — it holds no reference into the bus,
//! so there are no lifetimes to manage, no shared state between subscribers,
//! and no interior mutability. Like the bus, it must stay on one thread.

use std::collections::{BTreeMap, VecDeque};

/// One event delivered by an [`EventBus`].
///
/// `seq` is the bus-wide sequence number assigned by [`EventBus::publish`];
/// [`Event::new`] produces `seq == 0`, which also means "not yet published".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub topic: String,
    pub payload: String,
    /// Bus-wide, monotonically increasing sequence number assigned at
    /// [`EventBus::publish`] time. Events never share a sequence number, so
    /// `seq` totally orders every event the bus has published, across topics.
    pub seq: u64,
}

impl Event {
    pub fn new(topic: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
            seq: 0,
        }
    }
}

/// An in-process event hub.
///
/// [`EventBus::publish`] queues [`Event`]s per topic; the legacy
/// [`EventBus::pending`] / [`EventBus::drain`] API polls and removes them.
/// [`EventBus::subscribe`] creates a per-subscriber subscription that pulls
/// its own copy of the stream, and [`EventBus::log`] / [`EventBus::events_since`]
/// read the bus's bounded, non-consuming retained event log.
#[derive(Debug)]
pub struct EventBus {
    queues: BTreeMap<String, Vec<Event>>,
    logs: BTreeMap<String, VecDeque<Event>>,
    log_capacity: usize,
    next_seq: u64,
}

impl EventBus {
    /// Queues `event` for its topic and assigns it the next sequence number.
    ///
    /// The event is delivered to the topic's legacy pending queue (visible to
    /// [`EventBus::pending`] / [`EventBus::drain`]), to every
    /// [`EventSubscriber`] subscribed to the topic when they next pull, and —
    /// independently of all consumer paths — to the topic's retained event
    /// log, readable via [`EventBus::log`] / [`EventBus::events_since`]. When
    /// a publish would exceed the log's [`EventBus::log_capacity`], the oldest
    /// retained event for the topic is dropped first; the pending queue and
    /// subscribers are never affected by log eviction.
    pub fn publish(&mut self, mut event: Event) {
        event.seq = self.next_seq;
        self.next_seq += 1;
        let topic = event.topic.clone();
        self.queues
            .entry(topic.clone())
            .or_default()
            .push(event.clone());
        let log = self.logs.entry(topic).or_default();
        log.push_back(event);
        if log.len() > self.log_capacity {
            log.pop_front();
        }
    }

    /// Removes and returns every event pending for `topic`, in publish order.
    ///
    /// This is the legacy consumer path. Events removed here are no longer
    /// visible to subscribers pulling the same topic.
    pub fn drain(&mut self, topic: &str) -> Vec<Event> {
        self.queues.remove(topic).unwrap_or_default()
    }

    /// The number of events currently pending for `topic`.
    pub fn pending(&self, topic: &str) -> usize {
        self.queues.get(topic).map(Vec::len).unwrap_or(0)
    }

    /// The sequence number the next [`EventBus::publish`] will assign — i.e.
    /// how many events have been published to this bus so far.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Creates an [`EventBus`] whose per-topic retained event log holds at
    /// most `capacity` events per topic.
    ///
    /// A `capacity` of `0` disables retention: [`EventBus::log`] /
    /// [`EventBus::events_since`] always return empty, while the legacy
    /// pending queue and subscribers work exactly as usual.
    pub fn with_log_capacity(capacity: usize) -> Self {
        Self {
            queues: BTreeMap::new(),
            logs: BTreeMap::new(),
            log_capacity: capacity,
            next_seq: 0,
        }
    }

    /// The maximum number of events each topic's retained log holds.
    ///
    /// [`EventBus::with_log_capacity`] sets it; [`EventBus::default`] uses a
    /// finite default so the log is always bounded unless configured otherwise.
    pub fn log_capacity(&self) -> usize {
        self.log_capacity
    }

    /// The topic's full retained event log, oldest first, as copies.
    ///
    /// Returns every event published to `topic` that is still retained —
    /// dropping nothing from the bus, so the legacy pending queue, subscribers
    /// and later calls all observe the same events. The log is independent of
    /// consumption: events removed by [`EventBus::drain`] or delivered to
    /// subscribers remain readable here until capacity evicts them.
    pub fn log(&self, topic: impl Into<String>) -> Vec<Event> {
        self.logs
            .get(&topic.into())
            .map(|log| log.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The topic's retained events with `seq >= cursor`, oldest first, as
    /// copies — a non-consuming, cursor-based read of the event log.
    ///
    /// Pass the sequence number of the *next* event you want (e.g. the last
    /// consumed `seq + 1`) to resume a stream: every retained event whose
    /// `seq` is at least `cursor` is returned, and nothing is removed from the
    /// bus. If the log's oldest retained event has a `seq` greater than
    /// `cursor`, events between the cursor and the log were evicted by
    /// [`EventBus::log_capacity`]; [`EventBus::log_start`] reports the oldest
    /// retained sequence number so such gaps are detectable.
    pub fn events_since(&self, seq: u64, topic: impl Into<String>) -> Vec<Event> {
        let topic = topic.into();
        self.logs
            .get(&topic)
            .map(|log| {
                log.iter()
                    .filter(|event| event.seq >= seq)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many events are currently retained in `topic`'s log.
    pub fn log_len(&self, topic: &str) -> usize {
        self.logs.get(topic).map(VecDeque::len).unwrap_or(0)
    }

    /// The sequence number of `topic`'s oldest retained event, or `None` when
    /// the topic's log is empty (nothing published, or capacity `0`).
    ///
    /// A cursor reader can compare this with its cursor: a `log_start` greater
    /// than the cursor means events between the two were evicted by the log's
    /// capacity.
    pub fn log_start(&self, topic: &str) -> Option<u64> {
        self.logs
            .get(topic)
            .and_then(|log| log.front())
            .map(|event| event.seq)
    }

    /// Creates a per-subscriber subscription to `topic`.
    ///
    /// The returned [`EventSubscriber`] owns an independent queue and starts
    /// its watermark at the next sequence number, so it only ever receives
    /// events published *after* this call. It is not registered with the bus:
    /// delivery is pull-based via [`EventSubscriber::drain`], which reads the
    /// bus's pending events for the topic and copies them into the
    /// subscriber's own queue without modifying the bus.
    pub fn subscribe(&self, topic: impl Into<String>) -> EventSubscriber {
        EventSubscriber {
            topic: topic.into(),
            queue: VecDeque::new(),
            watermark: self.next_seq,
        }
    }
}

impl Default for EventBus {
    /// The default bus retains the newest `DEFAULT_LOG_CAPACITY` events per
    /// topic in its event log; the pending queue and subscription behaviour
    /// are identical to [`EventBus::with_log_capacity`].
    fn default() -> Self {
        Self::with_log_capacity(DEFAULT_LOG_CAPACITY)
    }
}

/// Default number of events each topic's retained log holds on a
/// [`EventBus::default`] bus.
const DEFAULT_LOG_CAPACITY: usize = 256;

/// A per-subscriber subscription to one [`EventBus`] topic.
///
/// Created by [`EventBus::subscribe`]. The subscriber owns an independent
/// queue and a watermark; it receives only events published to its topic after
/// it was created (or after the previous pull), and it holds no reference into
/// the bus, so it can outlive any borrow and needs no lifetime parameters.
#[derive(Debug)]
pub struct EventSubscriber {
    topic: String,
    queue: VecDeque<Event>,
    watermark: u64,
}

impl EventSubscriber {
    /// The topic this subscriber was created for.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Pulls the subscriber's pending events from `bus` into its independent
    /// queue and returns them in publish order, emptying the queue.
    ///
    /// Only events published to the subscribed topic since the previous pull
    /// are delivered — the subscriber's watermark advances to the bus's current
    /// sequence number, so a repeated `drain` without new publishes returns
    /// nothing and no event is ever delivered twice. The bus is left untouched:
    /// legacy [`EventBus::drain`] consumers and other subscribers observe the
    /// same events. Events already consumed from the bus via
    /// [`EventBus::drain`] are no longer visible to any subscriber.
    pub fn drain(&mut self, bus: &EventBus) -> Vec<Event> {
        if let Some(events) = bus.queues.get(&self.topic) {
            for event in events {
                if event.seq >= self.watermark {
                    self.queue.push_back(event.clone());
                }
            }
        }
        self.watermark = bus.next_seq;
        self.queue.drain(..).collect()
    }

    /// How many events a next call to [`EventSubscriber::drain`] would
    /// currently deliver: the bus's pending events for the subscribed topic
    /// that were published since the previous pull.
    pub fn pending(&self, bus: &EventBus) -> usize {
        bus.queues
            .get(&self.topic)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.seq >= self.watermark)
                    .count()
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_and_drains_events_by_topic() {
        let mut bus = EventBus::default();
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));
        assert_eq!(bus.pending("document.changed"), 2);
        assert_eq!(bus.drain("document.changed").len(), 2);
        assert_eq!(bus.pending("document.changed"), 0);
    }

    #[test]
    fn two_subscribers_each_receive_the_full_stream() {
        let mut bus = EventBus::default();
        let mut first = bus.subscribe("document.changed");
        let mut second = bus.subscribe("document.changed");
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));
        assert_eq!(first.pending(&bus), 2);
        assert_eq!(second.pending(&bus), 2);

        let from_first = first.drain(&bus);
        let from_second = second.drain(&bus);
        assert_eq!(from_first.len(), 2);
        assert_eq!(from_second.len(), 2);
        // Both subscribers observe exactly the same full stream.
        assert_eq!(from_first, from_second);
        assert_eq!(
            from_first
                .iter()
                .map(|event| event.payload.as_str())
                .collect::<Vec<_>>(),
            vec!["revision=1", "revision=2"]
        );

        // Pulls never modify the bus: the legacy view still sees both events.
        assert_eq!(bus.pending("document.changed"), 2);
        assert_eq!(bus.drain("document.changed").len(), 2);
    }

    #[test]
    fn legacy_publish_drain_pending_is_unchanged_alongside_subscribers() {
        let mut bus = EventBus::default();
        let mut subscriber = bus.subscribe("document.changed");
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));

        // Legacy semantics are untouched by the presence of a subscriber.
        assert_eq!(bus.pending("document.changed"), 2);
        assert_eq!(bus.drain("document.changed").len(), 2);
        assert_eq!(bus.pending("document.changed"), 0);
        assert_eq!(bus.drain("document.changed").len(), 0);

        // The subscriber never pulled, so it sees nothing and never affects
        // what the bus reported.
        assert_eq!(subscriber.pending(&bus), 0);
        assert_eq!(subscriber.drain(&bus).len(), 0);
    }

    #[test]
    fn subscriber_only_receives_events_published_after_subscription() {
        let mut bus = EventBus::default();
        bus.publish(Event::new("document.changed", "before"));
        let mut subscriber = bus.subscribe("document.changed");
        assert_eq!(subscriber.pending(&bus), 0);
        bus.publish(Event::new("document.changed", "after"));
        assert_eq!(subscriber.pending(&bus), 1);
        let got = subscriber.drain(&bus);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, "after");
    }

    #[test]
    fn drain_advances_the_watermark_without_duplicates() {
        let mut bus = EventBus::default();
        let mut subscriber = bus.subscribe("document.changed");
        bus.publish(Event::new("document.changed", "one"));
        assert_eq!(subscriber.drain(&bus).len(), 1);
        // The watermark advanced: nothing is re-delivered.
        assert_eq!(subscriber.pending(&bus), 0);
        assert_eq!(subscriber.drain(&bus).len(), 0);
        bus.publish(Event::new("document.changed", "two"));
        let got = subscriber.drain(&bus);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, "two");
    }

    #[test]
    fn subscriber_only_delivers_its_own_topic() {
        let mut bus = EventBus::default();
        let mut subscriber = bus.subscribe("document.changed");
        assert_eq!(subscriber.topic(), "document.changed");
        bus.publish(Event::new("document.changed", "changed"));
        bus.publish(Event::new("document.saved", "saved"));
        let got = subscriber.drain(&bus);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, "changed");
    }

    #[test]
    fn sequence_numbers_are_monotonic_across_topics() {
        let mut bus = EventBus::default();
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.saved", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));
        bus.publish(Event::new("task.finished", "build:0"));
        assert_eq!(bus.next_seq(), 4);

        // A subscriber created later only sees post-subscription events, with
        // strictly increasing sequence numbers even though topics interleave.
        let mut subscriber = bus.subscribe("document.changed");
        bus.publish(Event::new("task.output", "build:compiling"));
        bus.publish(Event::new("document.changed", "revision=3"));
        bus.publish(Event::new("document.changed", "revision=4"));
        let got = subscriber.drain(&bus);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].payload, "revision=3");
        assert_eq!(got[1].payload, "revision=4");
        assert!(got.windows(2).all(|pair| pair[0].seq < pair[1].seq));

        // Across every topic the bus's stream is totally ordered by seq: the
        // drained events sort to exactly the contiguous range 0..7.
        let mut all: Vec<Event> = bus.drain("document.changed");
        all.extend(bus.drain("document.saved"));
        all.extend(bus.drain("task.output"));
        all.extend(bus.drain("task.finished"));
        all.sort_by_key(|event| event.seq);
        assert_eq!(
            all.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn log_returns_copies_without_consuming_the_queue() {
        let mut bus = EventBus::default();
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));
        bus.publish(Event::new("document.changed", "revision=3"));

        // The log is a non-consuming read: the legacy queue still sees every
        // event, and the returned Vec holds copies in publish order.
        assert_eq!(bus.pending("document.changed"), 3);
        let log = bus.log("document.changed");
        assert_eq!(
            log.iter()
                .map(|event| event.payload.as_str())
                .collect::<Vec<_>>(),
            vec!["revision=1", "revision=2", "revision=3"]
        );
        assert_eq!(
            log.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(bus.pending("document.changed"), 3);

        // The log outlives consumption: draining the legacy queue does not
        // remove anything from the log.
        assert_eq!(bus.drain("document.changed").len(), 3);
        assert_eq!(bus.pending("document.changed"), 0);
        assert_eq!(bus.log("document.changed").len(), 3);
        // Unknown topics have an empty log.
        assert!(bus.log("document.saved").is_empty());
        assert_eq!(bus.log_len("document.saved"), 0);
    }

    #[test]
    fn events_since_resumes_a_stream_from_a_cursor() {
        let mut bus = EventBus::default();
        for revision in 0..5 {
            bus.publish(Event::new(
                "document.changed",
                format!("revision={revision}"),
            ));
        }

        // Inclusive cursor: everything at or after the cursor is returned,
        // and nothing is consumed.
        let resumed = bus.events_since(2, "document.changed");
        assert_eq!(
            resumed.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(bus.pending("document.changed"), 5);

        // Cursor at the end of the stream / beyond it is empty.
        assert!(bus.events_since(5, "document.changed").is_empty());
        assert!(bus.events_since(100, "document.changed").is_empty());
        // Cursor 0 is the whole retained log.
        assert_eq!(bus.events_since(0, "document.changed").len(), 5);
    }

    #[test]
    fn events_since_filters_by_topic_across_interleaved_streams() {
        let mut bus = EventBus::default();
        bus.publish(Event::new("document.changed", "a=1"));
        bus.publish(Event::new("document.saved", "s=1"));
        bus.publish(Event::new("document.changed", "a=2"));
        bus.publish(Event::new("task.output", "t=1"));
        bus.publish(Event::new("document.changed", "a=3"));

        // Each topic's log holds only its own events, in publish order, even
        // though the bus-wide seq stream interleaves topics.
        let changed = bus.log("document.changed");
        assert_eq!(
            changed.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
        assert_eq!(
            changed
                .iter()
                .map(|event| event.payload.as_str())
                .collect::<Vec<_>>(),
            vec!["a=1", "a=2", "a=3"]
        );
        assert_eq!(bus.log_len("document.saved"), 1);
        assert_eq!(bus.log_len("task.output"), 1);

        // A cursor read on one topic never leaks another topic's events, and
        // returns the topic's events with seq >= cursor in publish order.
        let since = bus.events_since(2, "document.changed");
        assert_eq!(
            since.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert_eq!(bus.events_since(0, "task.output").len(), 1);
        assert_eq!(bus.events_since(0, "document.saved").len(), 1);
    }

    #[test]
    fn log_capacity_evicts_the_oldest_events_only() {
        let mut bus = EventBus::with_log_capacity(3);
        for revision in 0..5 {
            bus.publish(Event::new(
                "document.changed",
                format!("revision={revision}"),
            ));
        }

        // Capacity 3 keeps exactly the newest 3; the oldest 2 are dropped.
        assert_eq!(bus.log_capacity(), 3);
        assert_eq!(bus.log_len("document.changed"), 3);
        assert_eq!(
            bus.log("document.changed")
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        // log_start reports the oldest retained seq, so the eviction of
        // seqs 0..2 is observable.
        assert_eq!(bus.log_start("document.changed"), Some(2));

        // The cursor read reflects the same eviction: events_since(0) starts
        // at the retained head (seq 2 > requested 0 = a detectable gap).
        let since = bus.events_since(0, "document.changed");
        assert_eq!(since[0].seq, 2);

        // Eviction is confined to the log: the legacy queue and subscribers
        // still see every published event.
        assert_eq!(bus.pending("document.changed"), 5);
        let mut subscriber = bus.subscribe("document.changed");
        bus.publish(Event::new("document.changed", "revision=5"));
        assert_eq!(subscriber.drain(&bus).len(), 1);
        // The newest publish pushed the log to seqs 3, 4, 5.
        assert_eq!(
            bus.log("document.changed")
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(bus.log_start("document.changed"), Some(3));
    }

    #[test]
    fn log_capacity_boundary_retains_exactly_capacity_events() {
        let mut bus = EventBus::with_log_capacity(3);
        for revision in 0..3 {
            bus.publish(Event::new(
                "document.changed",
                format!("revision={revision}"),
            ));
        }
        // Exactly at capacity: everything retained, nothing evicted.
        assert_eq!(bus.log_len("document.changed"), 3);
        assert_eq!(bus.log_start("document.changed"), Some(0));
        assert_eq!(bus.log("document.changed").len(), 3);

        // One more publish crosses the boundary: the oldest is evicted, the
        // log stays at exactly capacity.
        bus.publish(Event::new("document.changed", "revision=3"));
        assert_eq!(bus.log_len("document.changed"), 3);
        assert_eq!(bus.log_start("document.changed"), Some(1));
        assert_eq!(
            bus.log("document.changed")
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn zero_capacity_retains_nothing_but_legacy_api_keeps_working() {
        let mut bus = EventBus::with_log_capacity(0);
        bus.publish(Event::new("document.changed", "revision=1"));

        // Retention is fully disabled...
        assert_eq!(bus.log_capacity(), 0);
        assert!(bus.log("document.changed").is_empty());
        assert!(bus.events_since(0, "document.changed").is_empty());
        assert_eq!(bus.log_len("document.changed"), 0);
        assert_eq!(bus.log_start("document.changed"), None);

        // ...while the legacy queue is entirely unaffected.
        assert_eq!(bus.pending("document.changed"), 1);
        assert_eq!(bus.drain("document.changed")[0].payload, "revision=1");
    }

    #[test]
    fn default_bus_has_a_finite_log_capacity() {
        let mut bus = EventBus::default();
        assert_eq!(bus.log_capacity(), 256);
        for seq in 0..300 {
            bus.publish(Event::new("document.changed", format!("revision={seq}")));
        }
        // The default log is bounded: the oldest 300 - 256 = 44 events were
        // evicted, the newest 256 are retained.
        assert_eq!(bus.log_len("document.changed"), 256);
        assert_eq!(bus.log_start("document.changed"), Some(44));
        assert_eq!(bus.events_since(44, "document.changed").len(), 256);
        assert_eq!(bus.pending("document.changed"), 300);
    }

    #[test]
    fn event_log_coexists_with_subscribers_and_legacy_consumers() {
        let mut bus = EventBus::default();
        let mut subscriber = bus.subscribe("document.changed");
        bus.publish(Event::new("document.changed", "revision=1"));
        bus.publish(Event::new("document.changed", "revision=2"));

        // The subscriber and the legacy consumer each consume independently,
        // and the log observes the full stream either way.
        assert_eq!(subscriber.drain(&bus).len(), 2);
        assert_eq!(bus.drain("document.changed").len(), 2);
        assert_eq!(bus.pending("document.changed"), 0);
        assert_eq!(bus.log_len("document.changed"), 2);
        assert_eq!(bus.log("document.changed").len(), 2);

        // A later publish is visible to the log immediately, with a strictly
        // increasing seq that continues the bus-wide order.
        bus.publish(Event::new("document.changed", "revision=3"));
        let log = bus.log("document.changed");
        assert_eq!(
            log.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(log.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }
}

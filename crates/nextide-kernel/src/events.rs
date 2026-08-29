//! A tiny, dependency-free event hub with a legacy per-topic poll/drain API
//! and per-subscriber subscriptions.
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
/// its own copy of the stream.
#[derive(Default, Debug)]
pub struct EventBus {
    queues: BTreeMap<String, Vec<Event>>,
    next_seq: u64,
}

impl EventBus {
    /// Queues `event` for its topic and assigns it the next sequence number.
    ///
    /// The event is delivered both to the topic's legacy pending queue (visible
    /// to [`EventBus::pending`] / [`EventBus::drain`]) and, when they next pull,
    /// to every [`EventSubscriber`] subscribed to the topic.
    pub fn publish(&mut self, mut event: Event) {
        event.seq = self.next_seq;
        self.next_seq += 1;
        self.queues
            .entry(event.topic.clone())
            .or_default()
            .push(event);
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
}

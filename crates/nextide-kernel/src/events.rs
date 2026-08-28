use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub topic: String,
    pub payload: String,
}

impl Event {
    pub fn new(topic: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
        }
    }
}

#[derive(Default, Debug)]
pub struct EventBus {
    queues: BTreeMap<String, Vec<Event>>,
}

impl EventBus {
    pub fn publish(&mut self, event: Event) {
        self.queues
            .entry(event.topic.clone())
            .or_default()
            .push(event);
    }

    pub fn drain(&mut self, topic: &str) -> Vec<Event> {
        self.queues.remove(topic).unwrap_or_default()
    }

    pub fn pending(&self, topic: &str) -> usize {
        self.queues.get(topic).map(Vec::len).unwrap_or(0)
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
}

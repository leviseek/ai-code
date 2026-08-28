use crate::PlatformEvent;
use std::collections::VecDeque;

pub trait EventLoop {
    fn push(&mut self, event: PlatformEvent);
    fn next(&mut self) -> Option<PlatformEvent>;
}

#[derive(Default, Debug)]
pub struct HeadlessEventLoop {
    events: VecDeque<PlatformEvent>,
}

impl EventLoop for HeadlessEventLoop {
    fn push(&mut self, event: PlatformEvent) {
        self.events.push_back(event);
    }
    fn next(&mut self) -> Option<PlatformEvent> {
        self.events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_loop_preserves_event_order() {
        let mut loop_ = HeadlessEventLoop::default();
        loop_.push(PlatformEvent::CloseRequested);
        loop_.push(PlatformEvent::Resize {
            width: 1,
            height: 2,
        });
        assert!(matches!(loop_.next(), Some(PlatformEvent::CloseRequested)));
        assert!(matches!(loop_.next(), Some(PlatformEvent::Resize { .. })));
        assert!(loop_.next().is_none());
    }
}

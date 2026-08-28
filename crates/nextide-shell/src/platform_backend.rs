use crate::{EventLoop, HeadlessEventLoop};
use std::io;

pub trait PlatformBackend {
    type Loop: EventLoop;
    fn create_event_loop(&self) -> io::Result<Self::Loop>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessBackend;

impl PlatformBackend for HeadlessBackend {
    type Loop = HeadlessEventLoop;
    fn create_event_loop(&self) -> io::Result<Self::Loop> {
        Ok(HeadlessEventLoop::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlatformEvent;
    #[test]
    fn headless_backend_creates_an_empty_loop() {
        let backend = HeadlessBackend::default();
        let mut event_loop = backend.create_event_loop().unwrap();
        assert!(event_loop.next().is_none());
        let _ = PlatformEvent::CloseRequested;
    }
}

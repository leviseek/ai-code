use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowOptions {
    pub title: String,
    pub size: WindowSize,
    pub resizable: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "NextIDE".into(),
            size: WindowSize {
                width: 1280,
                height: 800,
            },
            resizable: true,
        }
    }
}

pub trait WindowService {
    fn create(&mut self, options: WindowOptions) -> io::Result<WindowHandle>;
    fn close(&mut self, window: &WindowHandle) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowHandle {
    pub id: u64,
}

#[derive(Default, Debug)]
pub struct HeadlessWindowService {
    next_id: u64,
}

impl WindowService for HeadlessWindowService {
    fn create(&mut self, _options: WindowOptions) -> io::Result<WindowHandle> {
        self.next_id += 1;
        Ok(WindowHandle { id: self.next_id })
    }
    fn close(&mut self, _window: &WindowHandle) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_service_creates_monotonic_handles() {
        let mut service = HeadlessWindowService::default();
        let first = service.create(WindowOptions::default()).unwrap();
        let second = service.create(WindowOptions::default()).unwrap();
        assert_eq!((first.id, second.id), (1, 2));
        service.close(&first).unwrap();
    }
}

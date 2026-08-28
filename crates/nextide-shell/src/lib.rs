use nextide_kernel::{
    ApplicationContext, HeadlessSurface, HeadlessWindowService, RenderSurface, SurfaceSize,
    WindowHandle, WindowOptions, WindowService,
};

#[derive(Debug)]
pub struct DesktopShell {
    pub app: ApplicationContext,
    pub title: String,
    pub windows: HeadlessWindowService,
    pub surface: HeadlessSurface,
    pub events: nextide_kernel::EventBus,
}

impl DesktopShell {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            app: ApplicationContext::bootstrap(""),
            title: title.into(),
            windows: HeadlessWindowService::default(),
            surface: HeadlessSurface::new(SurfaceSize {
                width: 1280,
                height: 800,
            }),
            events: nextide_kernel::EventBus::default(),
        }
    }

    pub fn create_main_window(&mut self) -> std::io::Result<WindowHandle> {
        let window = self.windows.create(WindowOptions {
            title: self.title.clone(),
            ..WindowOptions::default()
        })?;
        self.events.publish(nextide_kernel::Event::new(
            "window.created",
            window.id.to_string(),
        ));
        Ok(window)
    }

    pub fn scan_workspace<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        path: &std::path::Path,
    ) -> std::io::Result<()> {
        self.app.scan_workspace(fs, path)
    }

    pub fn open_file<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        path: &std::path::Path,
    ) -> std::io::Result<()> {
        self.app.open_file(fs, path)
    }

    pub fn save_file<F: nextide_kernel::FileSystem>(&mut self, fs: &F) -> std::io::Result<()> {
        self.app.save_file(fs)
    }

    pub fn close_file(&mut self, path: &std::path::Path, force: bool) -> std::io::Result<bool> {
        self.app.close_file(path, force)
    }

    pub fn present_frame(&mut self) -> std::io::Result<()> {
        self.surface.present()
    }

    pub fn resize_surface(&mut self, size: SurfaceSize) -> std::io::Result<()> {
        self.surface.resize(size)?;
        self.events.publish(nextide_kernel::Event::new(
            "window.resized",
            format!("{}x{}", size.width, size.height),
        ));
        Ok(())
    }

    pub fn close_window(&mut self, window: &WindowHandle) -> std::io::Result<()> {
        self.windows.close(window)?;
        self.events.publish(nextide_kernel::Event::new(
            "window.closed",
            window.id.to_string(),
        ));
        Ok(())
    }

    pub fn start(&self) -> String {
        format!(
            "{} ready ({} capabilities)",
            self.title,
            self.app.capabilities.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_publishes_window_lifecycle_events() {
        let mut shell = DesktopShell::new("NextIDE");
        let window = shell.create_main_window().unwrap();
        shell
            .resize_surface(SurfaceSize {
                width: 1000,
                height: 700,
            })
            .unwrap();
        shell.close_window(&window).unwrap();
        assert_eq!(shell.events.drain("window.created")[0].payload, "1");
        assert_eq!(shell.events.drain("window.resized")[0].payload, "1000x700");
        assert_eq!(shell.events.drain("window.closed")[0].payload, "1");
    }

    #[test]
    fn shell_starts_with_bootstrapped_application() {
        let shell = DesktopShell::new("NextIDE");
        assert_eq!(shell.start(), "NextIDE ready (3 capabilities)");
    }
}

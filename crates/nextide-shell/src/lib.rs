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

    pub fn present_frame(&mut self) -> std::io::Result<()> {
        self.surface.present()
    }

    pub fn resize_surface(&mut self, size: SurfaceSize) -> std::io::Result<()> {
        self.surface.resize(size)
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
    fn shell_starts_with_bootstrapped_application() {
        let shell = DesktopShell::new("NextIDE");
        assert_eq!(shell.start(), "NextIDE ready (3 capabilities)");
    }
}

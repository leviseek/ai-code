mod command_registry;
mod commands;
mod keymap;
mod notifications;
mod palette;
mod renderer;
mod view_model;

pub use command_registry::{command_id, search as search_commands, CommandDescriptor, COMMANDS};
pub use commands::{from_id as command_from_id, CommandResult, ShellCommand};
pub use keymap::{command_for, KeyChord};
pub use notifications::{Notification, NotificationCenter, NotificationLevel};
pub use palette::CommandPalette;
pub use renderer::{HeadlessRenderer, UiRenderer};
pub use view_model::ShellSnapshot;

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
    pub palette: CommandPalette,
    pub notifications: NotificationCenter,
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
            palette: CommandPalette::default(),
            notifications: NotificationCenter::default(),
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

    fn publish_state_changed(&mut self) {
        let snapshot = self.snapshot();
        self.events.publish(nextide_kernel::Event::new(
            "shell.state.changed",
            format!(
                "tabs={},views={},tree={}",
                snapshot.tabs, snapshot.editor_views, snapshot.tree_nodes
            ),
        ));
    }

    pub fn execute_checked<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        command: ShellCommand,
    ) -> std::io::Result<CommandResult> {
        match self.execute(fs, command) {
            Ok(result) => Ok(result),
            Err(error) => {
                let message = error.to_string();
                self.events.publish(nextide_kernel::Event::new(
                    "shell.command.failed",
                    message.clone(),
                ));
                self.notifications.push(NotificationLevel::Error, message);
                Err(error)
            }
        }
    }

    pub fn execute_selected<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        path: impl Into<std::path::PathBuf>,
    ) -> std::io::Result<CommandResult> {
        let Some(id) = self.selected_command_id() else {
            return Ok(CommandResult::Noop);
        };
        let Some(command) = command_from_id(id, path) else {
            return Ok(CommandResult::Noop);
        };
        let result = self.execute(fs, command)?;
        if result == CommandResult::Applied {
            self.events
                .publish(nextide_kernel::Event::new("palette.command.executed", id));
            self.palette.close();
        }
        Ok(result)
    }

    pub fn selected_command_id(&self) -> Option<&'static str> {
        self.palette.selected().map(|command| command.id)
    }

    pub fn scan_workspace<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        path: &std::path::Path,
    ) -> std::io::Result<()> {
        self.app.scan_workspace(fs, path)?;
        self.publish_state_changed();
        Ok(())
    }

    pub fn open_file<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        path: &std::path::Path,
    ) -> std::io::Result<()> {
        self.app.open_file(fs, path)?;
        self.publish_state_changed();
        Ok(())
    }

    pub fn save_file<F: nextide_kernel::FileSystem>(&mut self, fs: &F) -> std::io::Result<()> {
        self.app.save_file(fs)?;
        self.publish_state_changed();
        Ok(())
    }

    pub fn close_file(&mut self, path: &std::path::Path, force: bool) -> std::io::Result<bool> {
        let closed = self.app.close_file(path, force)?;
        if closed {
            self.publish_state_changed();
        }
        Ok(closed)
    }

    pub fn render_ui<R: UiRenderer>(&mut self, renderer: &mut R) -> std::io::Result<()> {
        renderer.render(&self.snapshot())?;
        self.surface.present()?;
        self.events
            .publish(nextide_kernel::Event::new("ui.frame.presented", "shell"));
        Ok(())
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

    pub fn open_palette(&mut self) {
        self.palette.open();
        self.events
            .publish(nextide_kernel::Event::new("palette.opened", "command"));
    }

    pub fn close_palette(&mut self) {
        self.palette.close();
        self.events
            .publish(nextide_kernel::Event::new("palette.closed", "command"));
    }

    pub fn execute<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        command: ShellCommand,
    ) -> std::io::Result<CommandResult> {
        let result = match command {
            ShellCommand::OpenFile(path) => {
                self.open_file(fs, &path)?;
                CommandResult::Applied
            }
            ShellCommand::CloseFile { path, force } => {
                if self.close_file(&path, force)? {
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
            ShellCommand::SaveActive => {
                if self.app.tabs.active().is_some() {
                    self.save_file(fs)?;
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
            ShellCommand::ScanWorkspace(path) => {
                self.scan_workspace(fs, &path)?;
                CommandResult::Applied
            }
            ShellCommand::ToggleDirectory(path) => {
                if self.app.file_tree.toggle(&path) {
                    self.publish_state_changed();
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
        };
        Ok(result)
    }

    pub fn snapshot(&self) -> ShellSnapshot {
        ShellSnapshot::from_context(
            self.title.clone(),
            &self.app,
            &self.palette,
            &self.notifications,
        )
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
    fn render_ui_renders_snapshot_and_presents_frame() {
        let mut shell = DesktopShell::new("NextIDE");
        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(renderer.frames(), 1);
        assert_eq!(shell.surface.frames_presented(), 1);
        assert_eq!(shell.events.pending("ui.frame.presented"), 1);
    }

    #[test]
    fn shell_snapshot_exposes_ui_state_counts() {
        let shell = DesktopShell::new("NextIDE");
        let snapshot = shell.snapshot();
        assert_eq!(snapshot.title, "NextIDE");
        assert_eq!(
            (snapshot.open_files, snapshot.tabs, snapshot.editor_views),
            (0, 0, 0)
        );
    }

    #[test]
    fn checked_command_publishes_failures() {
        let mut shell = DesktopShell::new("NextIDE");
        let path = std::path::PathBuf::from("main.rs");
        shell.app.workspace.open_file(path.clone());
        shell.app.tabs.open(path.clone());
        shell
            .app
            .views
            .push(nextide_kernel::EditorViewState::new(path.clone()));
        shell.app.views[0].mark_dirty(true);
        let error = shell
            .execute_checked(&NoopFs, ShellCommand::CloseFile { path, force: false })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(shell.events.pending("shell.command.failed"), 1);
    }

    struct NoopFs;
    impl nextide_kernel::FileSystem for NoopFs {
        fn read(&self, _: &std::path::Path) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn write(&self, _: &std::path::Path, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn exists(&self, _: &std::path::Path) -> bool {
            true
        }
    }

    #[test]
    fn shell_starts_with_bootstrapped_application() {
        let shell = DesktopShell::new("NextIDE");
        assert_eq!(shell.start(), "NextIDE ready (3 capabilities)");
    }
}

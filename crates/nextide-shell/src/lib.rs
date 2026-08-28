mod command_registry;
mod commands;
mod event_loop;
mod input;
mod keymap;
mod layout;
mod notifications;
mod palette;
mod platform_backend;
mod platform_events;
mod renderer;
mod view_model;

pub use command_registry::{command_id, search as search_commands, CommandDescriptor, COMMANDS};
pub use commands::{from_id as command_from_id, CommandResult, ShellCommand};
pub use event_loop::{EventLoop, HeadlessEventLoop};
pub use input::{command_for_input, UiInput};
pub use keymap::{command_for, KeyChord};
pub use layout::{Point, Rect, ShellLayout, UiRegion};
pub use notifications::{Notification, NotificationCenter, NotificationLevel};
pub use palette::CommandPalette;
pub use platform_backend::{HeadlessBackend, PlatformBackend};
pub use platform_events::{point as mouse_point, to_ui_input, PlatformEvent};
pub use renderer::{
    diff_tree_rows, tree_rows, HeadlessRenderer, TreeRenderRow, TreeRowChange, UiRenderer,
};
pub use view_model::{ShellSnapshot, TreeItem};

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
    pub layout: ShellLayout,
    pub running: bool,
    pub selected_tree: Option<std::path::PathBuf>,
    pub previous_tree_rows: Vec<TreeRenderRow>,
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
            layout: ShellLayout::default_1280x800(),
            running: true,
            selected_tree: None,
            previous_tree_rows: Vec::new(),
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

    pub fn hit_test(&self, point: Point) -> Option<UiRegion> {
        self.layout.hit_test(point)
    }

    pub fn tree_input_at(&mut self, point: Point) -> Option<UiInput> {
        let path = self.tree_item_at(point)?;
        self.selected_tree = Some(path.clone());
        self.events.publish(nextide_kernel::Event::new(
            "workspace.tree.selected",
            path.to_string_lossy(),
        ));
        let directory =
            self.app.file_tree.nodes().iter().any(|node| {
                node.path == path && node.kind == nextide_kernel::FileNodeKind::Directory
            });
        Some(if directory {
            UiInput::ClickDirectory(path)
        } else {
            UiInput::ClickFile(path)
        })
    }

    pub fn tree_item_at(&self, point: Point) -> Option<std::path::PathBuf> {
        if !self.layout.file_tree.contains(point) {
            return None;
        }
        let row = point.y / 24;
        self.app
            .file_tree
            .visible_nodes()
            .get(row as usize)
            .map(|node| node.path.clone())
    }

    pub fn run_frame<E: EventLoop, F: nextide_kernel::FileSystem, R: UiRenderer>(
        &mut self,
        event_loop: &mut E,
        fs: &F,
        renderer: &mut R,
    ) -> std::io::Result<Option<CommandResult>> {
        let result = self.pump_once(event_loop, fs)?;
        if result.is_some() {
            self.render_ui(renderer)?;
        }
        Ok(result)
    }

    pub fn run_until_idle<E: EventLoop, F: nextide_kernel::FileSystem>(
        &mut self,
        event_loop: &mut E,
        fs: &F,
    ) -> std::io::Result<usize> {
        let mut processed = 0;
        while self.running && self.pump_once(event_loop, fs)?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    pub fn pump_once<E: EventLoop, F: nextide_kernel::FileSystem>(
        &mut self,
        event_loop: &mut E,
        fs: &F,
    ) -> std::io::Result<Option<CommandResult>> {
        let Some(event) = event_loop.next() else {
            return Ok(None);
        };
        let input = match event {
            PlatformEvent::MouseClick { x, y } => self.tree_input_at(Point { x, y }),
            event => to_ui_input(event),
        };
        let Some(input) = input else {
            return Ok(Some(CommandResult::Noop));
        };
        Ok(Some(self.handle_input(fs, input)?))
    }

    pub fn handle_input<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        input: UiInput,
    ) -> std::io::Result<CommandResult> {
        match input {
            UiInput::Key(chord) if self.palette.is_open() && chord.key == '\n' => {
                self.execute_selected(fs, std::path::PathBuf::new())
            }
            UiInput::Key(chord) if self.palette.is_open() && chord.key == '\u{1b}' => {
                self.close_palette();
                Ok(CommandResult::Applied)
            }
            UiInput::Key(chord) if chord.ctrl && chord.key.eq_ignore_ascii_case(&'p') => {
                self.open_palette();
                Ok(CommandResult::Applied)
            }
            UiInput::Resize { width, height } => {
                self.resize_surface(SurfaceSize { width, height })?;
                Ok(CommandResult::Applied)
            }
            UiInput::Close => {
                self.running = false;
                self.events
                    .publish(nextide_kernel::Event::new("shell.closed", "requested"));
                Ok(CommandResult::Applied)
            }
            other => match command_for_input(other) {
                Some(command) => self.execute_checked(fs, command),
                None => Ok(CommandResult::Noop),
            },
        }
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
        let path = path.into();
        if path.as_os_str().is_empty() && id != "file.save" {
            return Ok(CommandResult::Noop);
        }
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
        let snapshot = self.snapshot();
        let rows = renderer.render_tree(&snapshot)?;
        let _changes = diff_tree_rows(&self.previous_tree_rows, &rows);
        self.previous_tree_rows = rows;
        renderer.render(&snapshot)?;
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
        self.layout = ShellLayout::for_size(size.width, size.height);
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
                let already_expanded = self
                    .app
                    .file_tree
                    .nodes()
                    .iter()
                    .any(|node| node.path == path && node.expanded);
                if (already_expanded && self.app.file_tree.toggle(&path))
                    || (!already_expanded && self.app.expand_directory(fs, &path)?)
                {
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
            self.selected_tree.clone(),
        )
    }

    pub fn is_running(&self) -> bool {
        self.running
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
    fn run_frame_processes_input_and_renders() {
        let mut shell = DesktopShell::new("NextIDE");
        let mut event_loop = HeadlessEventLoop::default();
        event_loop.push(PlatformEvent::Resize {
            width: 900,
            height: 600,
        });
        let mut renderer = HeadlessRenderer::default();
        assert_eq!(
            shell
                .run_frame(&mut event_loop, &NoopFs, &mut renderer)
                .unwrap(),
            Some(CommandResult::Applied)
        );
        assert_eq!(renderer.frames(), 1);
        assert_eq!(shell.surface.frames_presented(), 1);
    }

    #[test]
    fn run_until_idle_processes_all_queued_events() {
        let mut shell = DesktopShell::new("NextIDE");
        let mut events = HeadlessEventLoop::default();
        events.push(PlatformEvent::Resize {
            width: 900,
            height: 600,
        });
        events.push(PlatformEvent::KeyDown(KeyChord {
            ctrl: true,
            shift: false,
            alt: false,
            key: 'p',
        }));
        assert_eq!(shell.run_until_idle(&mut events, &NoopFs).unwrap(), 2);
        assert!(shell.palette.is_open());
    }

    #[test]
    fn tree_snapshot_marks_selected_item() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        let _ = shell.tree_input_at(Point { x: 10, y: 0 });
        let snapshot = shell.snapshot();
        assert_eq!(snapshot.selected_tree, Some("main.rs".into()));
        assert!(snapshot.visible_tree[0].selected);
    }

    #[test]
    fn tree_input_at_selects_and_classifies_visible_nodes() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("src", nextide_kernel::FileNodeKind::Directory);
        shell
            .app
            .file_tree
            .add("README.md", nextide_kernel::FileNodeKind::File);
        assert_eq!(
            shell.tree_input_at(Point { x: 20, y: 0 }),
            Some(UiInput::ClickFile("README.md".into()))
        );
        assert_eq!(shell.selected_tree, Some("README.md".into()));
        assert_eq!(shell.events.pending("workspace.tree.selected"), 1);
        assert_eq!(
            shell.tree_input_at(Point { x: 20, y: 24 }),
            Some(UiInput::ClickDirectory("src".into()))
        );
    }

    #[test]
    fn pump_once_routes_mouse_click_to_directory_command() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("src", nextide_kernel::FileNodeKind::Directory);
        let mut events = HeadlessEventLoop::default();
        events.push(PlatformEvent::MouseClick { x: 10, y: 0 });
        assert_eq!(
            shell.pump_once(&mut events, &TreeFs).unwrap(),
            Some(CommandResult::Applied)
        );
        assert!(
            shell
                .app
                .file_tree
                .nodes()
                .iter()
                .find(|node| node.path == std::path::PathBuf::from("src"))
                .unwrap()
                .expanded
        );
    }

    #[test]
    fn pump_once_routes_mouse_click_to_file_command() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        let mut events = HeadlessEventLoop::default();
        events.push(PlatformEvent::MouseClick { x: 10, y: 0 });
        assert_eq!(
            shell.pump_once(&mut events, &NoopFs).unwrap(),
            Some(CommandResult::Applied)
        );
        assert_eq!(shell.selected_tree, Some("main.rs".into()));
    }

    #[test]
    fn pump_once_translates_platform_events_to_shell_actions() {
        let mut shell = DesktopShell::new("NextIDE");
        let mut events = HeadlessEventLoop::default();
        events.push(PlatformEvent::Resize {
            width: 900,
            height: 600,
        });
        assert_eq!(
            shell.pump_once(&mut events, &NoopFs).unwrap(),
            Some(CommandResult::Applied)
        );
        assert_eq!(shell.surface.size.width, 900);
        assert!(shell.pump_once(&mut events, &NoopFs).unwrap().is_none());
    }

    #[test]
    fn handle_input_dispatches_palette_and_resize() {
        let mut shell = DesktopShell::new("NextIDE");
        assert_eq!(
            shell
                .handle_input(
                    &NoopFs,
                    UiInput::Key(KeyChord {
                        ctrl: true,
                        shift: false,
                        alt: false,
                        key: 'p'
                    })
                )
                .unwrap(),
            CommandResult::Applied
        );
        assert!(shell.palette.is_open());
        assert_eq!(
            shell
                .handle_input(
                    &NoopFs,
                    UiInput::Resize {
                        width: 900,
                        height: 600
                    }
                )
                .unwrap(),
            CommandResult::Applied
        );
        assert_eq!(shell.surface.size.width, 900);
        assert_eq!(shell.events.pending("window.resized"), 1);
    }

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
        shell
            .app
            .file_tree
            .add("src", nextide_kernel::FileNodeKind::Directory);
        shell
            .app
            .file_tree
            .add("src/main.rs", nextide_kernel::FileNodeKind::File);
        shell.app.file_tree.toggle(&"src".into());
        let _ = shell.tree_input_at(Point { x: 10, y: 24 });
        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(renderer.frames(), 1);
        assert_eq!(shell.surface.frames_presented(), 1);
        assert_eq!(shell.events.pending("ui.frame.presented"), 1);
        assert_eq!(renderer.tree_rows().len(), 2);
        assert_eq!(renderer.tree_rows()[1].x, 16);
        assert_eq!(renderer.tree_rows()[1].icon, "file");
        assert!(renderer.tree_rows()[1].selected);
        assert_eq!(renderer.tree_changes().len(), 2);
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

    struct TreeFs;
    impl nextide_kernel::FileSystem for TreeFs {
        fn read(&self, _: &std::path::Path) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn write(&self, _: &std::path::Path, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn exists(&self, _: &std::path::Path) -> bool {
            true
        }
        fn list(&self, _: &std::path::Path) -> std::io::Result<Vec<(std::path::PathBuf, bool)>> {
            Ok(vec![("src/main.rs".into(), false)])
        }
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
    fn close_input_stops_shell_lifecycle() {
        let mut shell = DesktopShell::new("NextIDE");
        assert!(shell.is_running());
        assert_eq!(
            shell.handle_input(&NoopFs, UiInput::Close).unwrap(),
            CommandResult::Applied
        );
        assert!(!shell.is_running());
        assert_eq!(shell.events.pending("shell.closed"), 1);
    }

    #[test]
    fn shell_starts_with_bootstrapped_application() {
        let shell = DesktopShell::new("NextIDE");
        assert_eq!(shell.start(), "NextIDE ready (3 capabilities)");
    }
}

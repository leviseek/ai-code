mod command_registry;
mod commands;
mod event_loop;
mod git_diff;
mod git_diff_panel;
mod git_status;
mod input;
mod keymap;
mod layout;
mod notifications;
mod palette;
mod platform_backend;
mod platform_events;
mod renderer;
mod view_model;

pub use command_registry::{
    command_id, command_ids, command_summaries, command_summary, command_title, describe_execution,
    get_command, is_registered, search as search_commands, CommandDescriptor, CommandSummary,
    COMMANDS,
};
pub use commands::{
    from_id as command_from_id, CommandError, CommandErrorKind, CommandExecutionResult,
    CommandOutcome, CommandResult, ShellCommand,
};
pub use event_loop::{EventLoop, HeadlessEventLoop};
pub use git_diff::{
    file_view_for, file_views, summary as diff_summary, summary_text as diff_summary_text,
    DefaultGitDiffAdapter, DiffFileView, DiffHunkView, DiffLineKind, DiffLineView, DiffSummary,
    GitDiffAdapter,
};
pub use git_diff_panel::DiffPanelSnapshot;
pub use git_status::{
    aggregate_directory_status, marker_for, status_kind, summary, summary_text, tree_statuses,
    DefaultGitStatusAdapter, GitStatusAdapter, GitStatusSummary, StatusKind, TreeStatus,
};
pub use input::{command_for_input, UiInput};
pub use keymap::{command_for, KeyChord};
pub use layout::{Point, Rect, ShellLayout, UiRegion};
pub use notifications::{Notification, NotificationCenter, NotificationLevel};
pub use palette::CommandPalette;
pub use platform_backend::{HeadlessBackend, PlatformBackend};
pub use platform_events::{point as mouse_point, to_ui_input, PlatformEvent};
pub use renderer::{
    diff_tree_rows, tree_change_counts, tree_rows, HeadlessRenderer, TreeChangeBatch,
    TreeRenderRow, TreeRowChange, UiRenderer,
};
pub use view_model::{ShellSnapshot, TreeItem};

use nextide_kernel::{
    ApplicationContext, GitDiffSnapshot, GitStatusSnapshot, HeadlessSurface, HeadlessWindowService,
    RenderSurface, SurfaceSize, WindowHandle, WindowOptions, WindowService,
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
    pub last_tree_changes: Vec<TreeRowChange>,
    pub tree_change_frame: u64,
    /// The externally supplied Git status snapshot, if any. Applied to
    /// snapshots and render rows by [`DesktopShell::snapshot`]; the shell
    /// never runs git itself, so `None` keeps every marker [`StatusKind::Clean`].
    /// Files are marked by exact path ([`marker_for`]); directories are marked
    /// with the aggregated status of everything below them
    /// ([`aggregate_directory_status`]).
    pub git_status: Option<GitStatusSnapshot>,
    /// The externally supplied diff panel snapshot, if any. Toggled and
    /// navigated by the pure state interface ([`DesktopShell::toggle_diff_file`],
    /// [`DesktopShell::diff_next_line`], [`DesktopShell::diff_prev_line`]) and
    /// by the `diff.*` commands; the shell never runs git itself, so `None`
    /// keeps every diff command a no-op.
    pub diff_panel: Option<DiffPanelSnapshot>,
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
            last_tree_changes: Vec::new(),
            tree_change_frame: 0,
            git_status: None,
            diff_panel: None,
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

    /// Executes a command through the observable path, returning a typed
    /// [`CommandExecutionResult`] that UI and plugins can consume. Every
    /// execution publishes a `shell.command.executed` event with a stable
    /// payload; failures additionally publish `shell.command.failed` and an
    /// error notification, matching [`execute_checked`]'s reporting.
    pub fn execute_observed<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        command: ShellCommand,
    ) -> CommandExecutionResult {
        let id = command_id(&command);
        let observed = CommandExecutionResult::from_io_result(id, self.execute(fs, command));
        self.events.publish(nextide_kernel::Event::new(
            "shell.command.executed",
            observed.to_event_payload(),
        ));
        if let Some(error) = observed.error() {
            let message = error.to_string();
            self.events.publish(nextide_kernel::Event::new(
                "shell.command.failed",
                message.clone(),
            ));
            self.notifications.push(NotificationLevel::Error, message);
        }
        observed
    }

    pub fn execute_checked<F: nextide_kernel::FileSystem>(
        &mut self,
        fs: &F,
        command: ShellCommand,
    ) -> std::io::Result<CommandResult> {
        self.execute_observed(fs, command).into_io_result()
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
        let observed = self.execute_observed(fs, command);
        if observed.is_applied() {
            self.events
                .publish(nextide_kernel::Event::new("palette.command.executed", id));
            self.palette.close();
        }
        observed.into_io_result()
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
        self.last_tree_changes = diff_tree_rows(&self.previous_tree_rows, &rows);
        self.previous_tree_rows = rows;
        renderer.render_tree_changes(&self.last_tree_changes)?;
        if !self.last_tree_changes.is_empty() {
            self.tree_change_frame += 1;
            let (added, removed, updated) = tree_change_counts(&self.last_tree_changes);
            self.events.publish(nextide_kernel::Event::new(
                "workspace.tree.changed",
                format!("added={added},removed={removed},updated={updated}"),
            ));
            self.events.publish(nextide_kernel::Event::new(
                "workspace.tree.change.batch",
                format!(
                    "frame={},added={added},removed={removed},updated={updated}",
                    self.tree_change_frame
                ),
            ));
            for change in &self.last_tree_changes {
                self.events.publish(nextide_kernel::Event::new(
                    "workspace.tree.change",
                    format!("{}:{}", change.kind(), change.path().to_string_lossy()),
                ));
            }
        }
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
            ShellCommand::DiffToggleFile(path) => {
                // An empty path (key binding) targets the selected tree path.
                let target = if path.as_os_str().is_empty() {
                    match &self.selected_tree {
                        Some(selected) => selected.clone(),
                        None => return Ok(CommandResult::Noop),
                    }
                } else {
                    path
                };
                if self.toggle_diff_file(&target) {
                    let expanded = self
                        .diff_panel
                        .as_ref()
                        .expect("toggle_diff_file just succeeded")
                        .is_expanded(&target);
                    self.events.publish(nextide_kernel::Event::new(
                        "diff.panel.changed",
                        format!(
                            "action=toggle,file={},expanded={}",
                            target.to_string_lossy(),
                            expanded
                        ),
                    ));
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
            ShellCommand::DiffNextLine => {
                if self.diff_next_line() {
                    self.publish_diff_selection_event();
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
            ShellCommand::DiffPrevLine => {
                if self.diff_prev_line() {
                    self.publish_diff_selection_event();
                    CommandResult::Applied
                } else {
                    CommandResult::Noop
                }
            }
        };
        Ok(result)
    }

    /// Publishes a stable `diff.panel.changed` event for a selection move:
    /// `action=select,index=<n>` when a visible line is selected, or
    /// `action=select,index=none` when the panel has no selection. Called only
    /// after a selection move that actually changed the panel.
    fn publish_diff_selection_event(&mut self) {
        let payload = match self
            .diff_panel
            .as_ref()
            .and_then(|panel| panel.selected_index())
        {
            Some(index) => format!("action=select,index={index}"),
            None => "action=select,index=none".to_string(),
        };
        self.events
            .publish(nextide_kernel::Event::new("diff.panel.changed", payload));
    }

    /// Applies an external Git status snapshot, making its markers visible in
    /// the next [`snapshot`](Self::snapshot) and rendered tree rows.
    ///
    /// The snapshot comes from outside the shell (for example from
    /// `nextide_kernel::git::status`); this method never runs git itself.
    /// File nodes are matched exactly, as in [`marker_for`]: an entry whose
    /// path equals a visible file node's path marks that node. Directory nodes
    /// are marked with the aggregated status of every entry below them, as in
    /// [`aggregate_directory_status`] (the most severe descendant kind, or
    /// the directory's own entry when it has one, for example a submodule);
    /// a directory with no changed entry under it stays [`StatusKind::Clean`],
    /// as does every node when no Git snapshot has been applied. The
    /// snapshot's [`summary`] is exposed as [`ShellSnapshot::git_summary`]
    /// for the status bar. A later call replaces the previous snapshot.
    pub fn update_git_status(&mut self, snapshot: GitStatusSnapshot) {
        self.git_status = Some(snapshot);
    }

    /// Applies an external diff snapshot, replacing the current diff panel.
    ///
    /// The snapshot comes from outside the shell (for example from
    /// `nextide_kernel::git::diff`); this method never runs git itself. The
    /// panel starts with every file expanded and nothing selected, per
    /// [`DiffPanelSnapshot::from_snapshot`], and its summary is exposed as
    /// [`ShellSnapshot::diff_summary`] for the status bar. A later call
    /// replaces the previous panel.
    pub fn update_diff_panel(&mut self, snapshot: GitDiffSnapshot) {
        self.diff_panel = Some(DiffPanelSnapshot::from_snapshot(&snapshot));
    }

    /// The current diff panel, if one has been applied.
    pub fn diff_panel_ref(&self) -> Option<&DiffPanelSnapshot> {
        self.diff_panel.as_ref()
    }

    /// Toggles the expansion of one diff panel file, returning whether the
    /// panel state changed.
    ///
    /// Pure state interface: `false` when no panel is active or the panel
    /// does not report `path` (matching [`DiffPanelSnapshot::toggle_file`]).
    pub fn toggle_diff_file(&mut self, path: &std::path::Path) -> bool {
        let Some(panel) = self.diff_panel.as_mut() else {
            return false;
        };
        panel.toggle_file(path)
    }

    /// Moves the diff panel selection to the next visible line, returning
    /// whether the selection actually moved.
    ///
    /// Pure state interface: `false` when no panel is active, when nothing is
    /// visible, or when the selection is already clamped at the last visible
    /// line. From no selection it selects the first visible line.
    pub fn diff_next_line(&mut self) -> bool {
        let Some(panel) = self.diff_panel.as_mut() else {
            return false;
        };
        let before = panel.selected_index();
        panel.select_next();
        panel.selected_index() != before
    }

    /// Moves the diff panel selection to the previous visible line, returning
    /// whether the selection actually moved.
    ///
    /// Pure state interface: `false` when no panel is active, when nothing is
    /// visible, or when the selection is already clamped at the first visible
    /// line. From no selection it selects the last visible line.
    pub fn diff_prev_line(&mut self) -> bool {
        let Some(panel) = self.diff_panel.as_mut() else {
            return false;
        };
        let before = panel.selected_index();
        panel.select_prev();
        panel.selected_index() != before
    }

    pub fn snapshot(&self) -> ShellSnapshot {
        let mut snapshot = ShellSnapshot::from_context(
            self.title.clone(),
            &self.app,
            &self.palette,
            &self.notifications,
            self.selected_tree.clone(),
        );
        if let Some(git) = self.git_status.as_ref() {
            snapshot.git_summary = Some(summary(git));
            // Files carry their exact-path marker; directories carry the
            // aggregated status of everything below them (via the pure
            // `aggregate_directory_status` projection), so a directory whose
            // children changed shows the most severe descendant kind instead
            // of staying `Clean`. Porcelain names files, not directories, so a
            // directory only ever changes because its children changed (or
            // because it has its own entry, for example a submodule).
            let statuses = tree_statuses(git);
            for item in &mut snapshot.visible_tree {
                item.status = if item.directory {
                    aggregate_directory_status(&statuses, &item.path).unwrap_or(StatusKind::Clean)
                } else {
                    marker_for(git, &item.path).unwrap_or(StatusKind::Clean)
                };
            }
        }
        if let Some(panel) = self.diff_panel.as_ref() {
            snapshot.diff_summary = Some(panel.summary());
        }
        snapshot
    }

    pub fn tree_changes(&self) -> &[TreeRowChange] {
        &self.last_tree_changes
    }

    pub fn tree_change_frame(&self) -> u64 {
        self.tree_change_frame
    }

    pub fn tree_change_batch(&self) -> TreeChangeBatch {
        TreeChangeBatch {
            frame: self.tree_change_frame,
            changes: self.last_tree_changes.clone(),
        }
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
    use nextide_kernel::git::{GitFileState, GitStatusCode, GitStatusEntry};
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
                .find(|node| node.path == std::path::Path::new("src"))
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
        assert_eq!(shell.tree_changes().len(), 2);
        assert_eq!(shell.tree_change_frame(), 1);
        assert_eq!(
            shell.events.drain("workspace.tree.change.batch")[0].payload,
            "frame=1,added=2,removed=0,updated=0"
        );
        assert_eq!(
            shell.events.drain("workspace.tree.changed")[0].payload,
            "added=2,removed=0,updated=0"
        );
        shell.render_ui(&mut renderer).unwrap();
        assert!(shell.tree_changes().is_empty());
        assert_eq!(shell.events.pending("workspace.tree.changed"), 0);
        shell.tree_input_at(Point { x: 10, y: 0 });
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(shell.tree_changes().len(), 2);
        assert_eq!(
            shell.events.drain("workspace.tree.changed")[0].payload,
            "added=0,removed=0,updated=2"
        );
        assert_eq!(shell.tree_change_frame(), 2);
        assert_eq!(
            shell.events.drain("workspace.tree.change.batch")[0].payload,
            "frame=2,added=0,removed=0,updated=2"
        );
        let changes = shell.events.drain("workspace.tree.change");
        assert_eq!(changes.len(), 4);
        assert_eq!(
            changes
                .iter()
                .filter(|event| event.payload.starts_with("added:"))
                .count(),
            2
        );
        assert_eq!(
            changes
                .iter()
                .filter(|event| event.payload.starts_with("updated:"))
                .count(),
            2
        );
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
        assert_eq!(shell.events.pending("shell.command.executed"), 1);
    }

    #[test]
    fn execute_observed_publishes_typed_execution_result() {
        let mut shell = DesktopShell::new("NextIDE");
        let result = shell.execute_observed(&NoopFs, ShellCommand::SaveActive);
        assert!(result.is_noop());
        assert_eq!(result.command_id(), "file.save");
        assert_eq!(result.outcome(), CommandOutcome::Noop);
        assert!(result.error().is_none());
        assert_eq!(shell.events.pending("shell.command.executed"), 1);
        assert_eq!(shell.events.pending("shell.command.failed"), 0);
        assert_eq!(
            shell.events.drain("shell.command.executed")[0].payload,
            "id=file.save,outcome=noop"
        );
    }

    #[test]
    fn execute_observed_returns_typed_failure_and_publishes_events() {
        let mut shell = DesktopShell::new("NextIDE");
        let path = std::path::PathBuf::from("main.rs");
        shell.app.workspace.open_file(path.clone());
        shell.app.tabs.open(path.clone());
        shell
            .app
            .views
            .push(nextide_kernel::EditorViewState::new(path.clone()));
        shell.app.views[0].mark_dirty(true);
        let result =
            shell.execute_observed(&NoopFs, ShellCommand::CloseFile { path, force: false });
        assert!(result.is_failed());
        assert!(!result.is_success());
        assert_eq!(result.error().unwrap().kind(), CommandErrorKind::Io);
        assert_eq!(shell.events.pending("shell.command.executed"), 1);
        assert_eq!(shell.events.pending("shell.command.failed"), 1);
        assert_eq!(shell.notifications.items().len(), 1);
        assert_eq!(
            shell.notifications.items()[0].level,
            NotificationLevel::Error
        );
        let payload = shell.events.drain("shell.command.executed")[0]
            .payload
            .clone();
        assert!(payload.starts_with("id=file.close,outcome=failed,error=io,message="));
        assert_eq!(
            shell.events.drain("shell.command.failed")[0].payload,
            "document has unsaved changes"
        );
    }

    struct TreeFs;
    impl nextide_kernel::FileSystem for TreeFs {
        fn read(&self, _: &std::path::Path) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn write(&self, _: &std::path::Path, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn exists(&self, _: &std::path::Path) -> std::io::Result<bool> {
            Ok(true)
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
        fn exists(&self, _: &std::path::Path) -> std::io::Result<bool> {
            Ok(true)
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

    /// Maps one porcelain byte to its kernel code (mirrors the kernel's
    /// private parser so tests can build any XY combination without git).
    fn code(byte: u8) -> GitStatusCode {
        match byte {
            b' ' => GitStatusCode::Unmodified,
            b'M' => GitStatusCode::Modified,
            b'A' => GitStatusCode::Added,
            b'D' => GitStatusCode::Deleted,
            b'?' => GitStatusCode::Untracked,
            b'U' => GitStatusCode::Unmerged,
            other => GitStatusCode::Unknown(other as char),
        }
    }

    fn status_entry(path: &str, xy: &str) -> GitStatusEntry {
        let bytes = xy.as_bytes();
        assert_eq!(bytes.len(), 2, "XY code must be two bytes");
        GitStatusEntry {
            state: GitFileState::new(code(bytes[0]), code(bytes[1])),
            path: path.to_string(),
            original: None,
        }
    }

    fn status_snapshot(entries: Vec<GitStatusEntry>) -> GitStatusSnapshot {
        GitStatusSnapshot { entries }
    }

    #[test]
    fn update_git_status_marks_snapshot_and_render_rows() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        shell
            .app
            .file_tree
            .add("notes.md", nextide_kernel::FileNodeKind::File);
        shell.update_git_status(status_snapshot(vec![
            status_entry("main.rs", "M "),
            status_entry("notes.md", "??"),
        ]));

        let snapshot = shell.snapshot();
        assert_eq!(
            snapshot.git_summary,
            Some(GitStatusSummary {
                total: 2,
                staged: 1,
                unstaged: 0,
                untracked: 1,
                conflicted: 0,
            })
        );
        assert_eq!(snapshot.visible_tree[0].status, StatusKind::Staged);
        assert_eq!(snapshot.visible_tree[1].status, StatusKind::Untracked);

        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(renderer.tree_rows()[0].status, StatusKind::Staged);
        assert_eq!(renderer.tree_rows()[1].status, StatusKind::Untracked);
        // The short badge reaches render rows through the same projection.
        assert_eq!(renderer.tree_rows()[0].status_marker, "S");
        assert_eq!(renderer.tree_rows()[1].status_marker, "?");
    }

    #[test]
    fn update_git_status_replaces_the_previous_snapshot() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        shell.update_git_status(status_snapshot(vec![status_entry("main.rs", "MM")]));
        assert_eq!(
            shell.snapshot().visible_tree[0].status,
            StatusKind::StagedAndUnstaged
        );

        // A clean replacement clears the markers but keeps a (zero) summary:
        // the snapshot exists, it just has no entries.
        shell.update_git_status(status_snapshot(Vec::new()));
        let snapshot = shell.snapshot();
        assert_eq!(snapshot.visible_tree[0].status, StatusKind::Clean);
        assert_eq!(snapshot.git_summary, Some(GitStatusSummary::default()));
    }

    #[test]
    fn git_status_text_accessor_projects_the_applied_summary() {
        let mut shell = DesktopShell::new("NextIDE");
        // No Git snapshot applied yet: no status-bar text at all.
        assert_eq!(shell.snapshot().git_status_text(), None);

        shell.update_git_status(status_snapshot(vec![status_entry("main.rs", "M ")]));
        assert_eq!(
            shell.snapshot().git_status_text().as_deref(),
            Some("changed: staged=1 unstaged=0 untracked=0")
        );

        // A clean replacement keeps Git integration active and reports the
        // clean text, not None.
        shell.update_git_status(status_snapshot(Vec::new()));
        assert_eq!(shell.snapshot().git_status_text().as_deref(), Some("clean"));

        // Conflicts win the state word and keep their own bucket.
        shell.update_git_status(status_snapshot(vec![status_entry("main.rs", "UU")]));
        assert_eq!(
            shell.snapshot().git_status_text().as_deref(),
            Some("conflicted: conflicted=1 staged=0 unstaged=0 untracked=0")
        );
    }

    #[test]
    fn git_status_is_optional_and_defaults_to_clean() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        assert!(shell.git_status.is_none());

        let snapshot = shell.snapshot();
        assert_eq!(snapshot.git_summary, None);
        assert_eq!(snapshot.visible_tree[0].status, StatusKind::Clean);

        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(renderer.tree_rows()[0].status, StatusKind::Clean);
    }

    #[test]
    fn marker_changes_surface_as_updated_rows() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(shell.tree_changes().len(), 1);
        assert!(matches!(&shell.tree_changes()[0], TreeRowChange::Added(_)));

        shell.update_git_status(status_snapshot(vec![status_entry("main.rs", " M")]));
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(shell.tree_changes().len(), 1);
        assert!(
            matches!(&shell.tree_changes()[0], TreeRowChange::Updated(row) if row.status == StatusKind::Unstaged && row.status_marker == "U")
        );
        assert_eq!(shell.tree_change_frame(), 2);
    }

    /// A visible tree with `src/` (expanded, so its children are visible),
    /// `src/main.rs` and a top-level `notes.md`.
    fn shell_with_src_tree() -> DesktopShell {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("src", nextide_kernel::FileNodeKind::Directory);
        shell
            .app
            .file_tree
            .add("src/main.rs", nextide_kernel::FileNodeKind::File);
        shell
            .app
            .file_tree
            .add("notes.md", nextide_kernel::FileNodeKind::File);
        shell.app.file_tree.toggle(&std::path::PathBuf::from("src"));
        shell
    }

    #[test]
    fn directory_markers_aggregate_descendant_status_in_snapshot_and_rows() {
        let mut shell = shell_with_src_tree();
        shell.update_git_status(status_snapshot(vec![
            status_entry("src/main.rs", "M "),
            status_entry("notes.md", "??"),
        ]));

        let snapshot = shell.snapshot();
        // Sorted by path: notes.md, src, src/main.rs. The file keeps its exact
        // marker; the directory aggregates the change under it.
        assert_eq!(snapshot.visible_tree[0].status, StatusKind::Untracked);
        assert_eq!(snapshot.visible_tree[1].status, StatusKind::Staged);
        assert_eq!(snapshot.visible_tree[2].status, StatusKind::Staged);
        assert_eq!(
            snapshot.git_summary,
            Some(GitStatusSummary {
                total: 2,
                staged: 1,
                unstaged: 0,
                untracked: 1,
                conflicted: 0,
            })
        );

        // The badge reaches render rows through the same projection.
        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(renderer.tree_rows()[1].status, StatusKind::Staged);
        assert_eq!(renderer.tree_rows()[1].status_marker, "S");
    }

    #[test]
    fn directory_marker_uses_the_most_severe_descendant_kind() {
        let mut shell = shell_with_src_tree();
        shell.update_git_status(status_snapshot(vec![
            status_entry("src/a.rs", "UU"),
            status_entry("src/b.rs", "M "),
        ]));
        let snapshot = shell.snapshot();
        // A conflicted child outranks a staged child in the aggregation.
        let src = snapshot
            .visible_tree
            .iter()
            .find(|item| item.path == std::path::Path::new("src"))
            .unwrap();
        assert_eq!(src.status, StatusKind::Conflicted);
        assert_eq!(src.status.marker(), "!");
    }

    #[test]
    fn directory_without_changed_descendants_stays_clean() {
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("vendor", nextide_kernel::FileNodeKind::Directory);
        shell
            .app
            .file_tree
            .add("vendor/lib.rs", nextide_kernel::FileNodeKind::File);
        shell
            .app
            .file_tree
            .toggle(&std::path::PathBuf::from("vendor"));
        shell
            .app
            .file_tree
            .add("main.rs", nextide_kernel::FileNodeKind::File);
        shell.update_git_status(status_snapshot(vec![status_entry("main.rs", " M")]));

        let snapshot = shell.snapshot();
        // "main.rs" sorts before "vendor": the changed file is marked, the
        // directory without changed descendants stays Clean.
        assert_eq!(snapshot.visible_tree[0].status, StatusKind::Unstaged);
        let vendor = snapshot
            .visible_tree
            .iter()
            .find(|item| item.path == std::path::Path::new("vendor"))
            .unwrap();
        assert_eq!(vendor.status, StatusKind::Clean);
    }

    #[test]
    fn directory_with_its_own_entry_keeps_that_kind() {
        // A directory-level porcelain entry (for example a submodule) marks
        // the directory itself even without any child entries.
        let mut shell = DesktopShell::new("NextIDE");
        shell
            .app
            .file_tree
            .add("vendor", nextide_kernel::FileNodeKind::Directory);
        shell.update_git_status(status_snapshot(vec![status_entry("vendor", "M ")]));
        assert_eq!(shell.snapshot().visible_tree[0].status, StatusKind::Staged);
    }

    #[test]
    fn clean_replacement_clears_directory_markers() {
        let mut shell = shell_with_src_tree();
        shell.update_git_status(status_snapshot(vec![status_entry("src/main.rs", "M ")]));
        assert_eq!(shell.snapshot().visible_tree[1].status, StatusKind::Staged);

        // A clean replacement clears the directory markers too, keeping the
        // (zero) summary: the snapshot exists, it just has no entries.
        shell.update_git_status(status_snapshot(Vec::new()));
        let snapshot = shell.snapshot();
        assert_eq!(snapshot.visible_tree[1].status, StatusKind::Clean);
        assert_eq!(snapshot.git_summary, Some(GitStatusSummary::default()));
    }

    #[test]
    fn directory_marker_change_surfaces_as_updated_rows() {
        let mut shell = shell_with_src_tree();
        let mut renderer = HeadlessRenderer::default();
        shell.render_ui(&mut renderer).unwrap();
        assert_eq!(shell.tree_changes().len(), 3);
        assert!(shell
            .tree_changes()
            .iter()
            .all(|change| matches!(change, TreeRowChange::Added(_))));

        // Applying a change under src moves both the file and the directory
        // from Clean to a badge, so both surface as Updated rows.
        shell.update_git_status(status_snapshot(vec![status_entry("src/main.rs", " M")]));
        shell.render_ui(&mut renderer).unwrap();
        let src = shell
            .tree_changes()
            .iter()
            .find(|change| change.path() == std::path::Path::new("src"))
            .expect("src must change");
        assert!(
            matches!(src, TreeRowChange::Updated(row) if row.status == StatusKind::Unstaged && row.status_marker == "U")
        );
        let file = shell
            .tree_changes()
            .iter()
            .find(|change| change.path() == std::path::Path::new("src/main.rs"))
            .expect("src/main.rs must change");
        assert!(
            matches!(file, TreeRowChange::Updated(row) if row.status == StatusKind::Unstaged && row.status_marker == "U")
        );
    }

    // Diff panel helpers: build kernel diff snapshots without git.

    fn diff_line(
        kind: nextide_kernel::GitDiffLineKind,
        old: Option<u32>,
        new: Option<u32>,
        text: &str,
    ) -> nextide_kernel::GitDiffLine {
        nextide_kernel::GitDiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: text.to_string(),
        }
    }

    fn diff_hunk(
        header: &str,
        lines: Vec<nextide_kernel::GitDiffLine>,
    ) -> nextide_kernel::GitDiffHunk {
        nextide_kernel::GitDiffHunk {
            header: header.to_string(),
            lines,
        }
    }

    fn diff_file(
        path: &str,
        binary: bool,
        hunks: Vec<nextide_kernel::GitDiffHunk>,
    ) -> nextide_kernel::GitDiffFile {
        nextide_kernel::GitDiffFile {
            path: path.to_string(),
            binary,
            hunks,
        }
    }

    fn diff_snapshot(files: Vec<nextide_kernel::GitDiffFile>) -> nextide_kernel::GitDiffSnapshot {
        nextide_kernel::GitDiffSnapshot { files }
    }

    /// One small two-file diff: `docs/notes.md` (2 lines) and `src/lib.rs`
    /// (3 lines), plus a binary `assets/logo.png` with no lines.
    fn sample_diff() -> nextide_kernel::GitDiffSnapshot {
        diff_snapshot(vec![
            diff_file(
                "docs/notes.md",
                false,
                vec![diff_hunk(
                    "@@ -1,2 +1,1 @@",
                    vec![
                        diff_line(
                            nextide_kernel::GitDiffLineKind::Context,
                            Some(1),
                            Some(1),
                            "title",
                        ),
                        diff_line(
                            nextide_kernel::GitDiffLineKind::Removed,
                            Some(2),
                            None,
                            "obsolete line",
                        ),
                    ],
                )],
            ),
            diff_file(
                "src/lib.rs",
                false,
                vec![diff_hunk(
                    "@@ -1,3 +1,3 @@",
                    vec![
                        diff_line(
                            nextide_kernel::GitDiffLineKind::Context,
                            Some(1),
                            Some(1),
                            "use std::io;",
                        ),
                        diff_line(
                            nextide_kernel::GitDiffLineKind::Added,
                            None,
                            Some(2),
                            "let x = 2;",
                        ),
                        diff_line(
                            nextide_kernel::GitDiffLineKind::Removed,
                            Some(2),
                            None,
                            "let x = 1;",
                        ),
                    ],
                )],
            ),
            diff_file("assets/logo.png", true, Vec::new()),
        ])
    }

    fn key(ctrl: bool, key: char) -> UiInput {
        UiInput::Key(KeyChord {
            ctrl,
            shift: false,
            alt: false,
            key,
        })
    }

    #[test]
    fn diff_panel_is_optional_and_defaults_to_none() {
        let mut shell = DesktopShell::new("NextIDE");
        assert!(shell.diff_panel.is_none());
        assert_eq!(shell.diff_panel_ref(), None);

        let snapshot = shell.snapshot();
        assert_eq!(snapshot.diff_summary, None);
        assert_eq!(snapshot.diff_status_text(), None);

        // Every pure state interface method is a no-op without a panel.
        assert!(!shell.toggle_diff_file(std::path::Path::new("src/lib.rs")));
        assert!(!shell.diff_next_line());
        assert!(!shell.diff_prev_line());

        // And every diff command reports Noop.
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffToggleFile("src/lib.rs".into()))
                .unwrap(),
            CommandResult::Noop
        );
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffNextLine)
                .unwrap(),
            CommandResult::Noop
        );
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffPrevLine)
                .unwrap(),
            CommandResult::Noop
        );
    }

    #[test]
    fn update_diff_panel_exposes_summary_in_snapshot() {
        let mut shell = DesktopShell::new("NextIDE");
        let diff = sample_diff();
        shell.update_diff_panel(diff.clone());

        let panel = shell.diff_panel_ref().expect("panel should be applied");
        assert_eq!(panel.file_count(), 3);
        assert!(panel.is_expanded(std::path::Path::new("src/lib.rs")));
        assert_eq!(panel.visible_line_count(), 5);
        assert_eq!(panel.selected_index(), None);
        assert_eq!(panel.summary(), diff_summary(&diff));

        let snapshot = shell.snapshot();
        assert_eq!(snapshot.diff_summary, Some(panel.summary()));
        assert_eq!(
            snapshot.diff_status_text().as_deref(),
            Some("changed: files=3 added=1 removed=2 context=2 binary_files=1")
        );
    }

    #[test]
    fn update_diff_panel_replaces_the_previous_panel() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());
        assert_eq!(shell.diff_panel_ref().unwrap().visible_line_count(), 5);

        // A clean replacement keeps the panel active with no lines.
        shell.update_diff_panel(diff_snapshot(Vec::new()));
        let panel = shell.diff_panel_ref().unwrap();
        assert_eq!(panel.file_count(), 0);
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(panel.selected_index(), None);
        assert_eq!(
            shell.snapshot().diff_status_text().as_deref(),
            Some("clean")
        );
    }

    #[test]
    fn diff_toggle_file_command_toggles_and_publishes_events() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());

        let path = std::path::PathBuf::from("src/lib.rs");
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffToggleFile(path.clone()))
                .unwrap(),
            CommandResult::Applied
        );
        assert!(!shell.diff_panel_ref().unwrap().is_expanded(&path));
        assert_eq!(shell.diff_panel_ref().unwrap().visible_line_count(), 2);
        assert_eq!(
            shell.events.drain("diff.panel.changed")[0].payload,
            "action=toggle,file=src/lib.rs,expanded=false"
        );

        // Toggling back restores the lines.
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffToggleFile(path.clone()))
                .unwrap(),
            CommandResult::Applied
        );
        assert!(shell.diff_panel_ref().unwrap().is_expanded(&path));
        assert_eq!(
            shell.events.drain("diff.panel.changed")[0].payload,
            "action=toggle,file=src/lib.rs,expanded=true"
        );

        // Unknown paths are Noop and publish nothing.
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffToggleFile("Cargo.toml".into()))
                .unwrap(),
            CommandResult::Noop
        );
        assert_eq!(shell.events.pending("diff.panel.changed"), 0);
    }

    #[test]
    fn diff_line_commands_move_selection_and_publish_events() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());
        let panel = shell.diff_panel_ref().unwrap();
        assert_eq!(panel.visible_line_count(), 5);

        // Next from no selection picks the first visible line.
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffNextLine)
                .unwrap(),
            CommandResult::Applied
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(0));
        assert_eq!(
            shell.events.drain("diff.panel.changed")[0].payload,
            "action=select,index=0"
        );

        // Walk to the end; the last next clamps and reports Noop.
        for expected in 1..5 {
            assert_eq!(
                shell
                    .execute_checked(&NoopFs, ShellCommand::DiffNextLine)
                    .unwrap(),
                CommandResult::Applied
            );
            assert_eq!(
                shell.diff_panel_ref().unwrap().selected_index(),
                Some(expected)
            );
        }
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffNextLine)
                .unwrap(),
            CommandResult::Noop
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(4));
        // Each applied move published exactly one event; the clamped Noop
        // published none. (The initial select event was drained above.)
        assert_eq!(shell.events.drain("diff.panel.changed").len(), 4);

        // Prev walks back and clamps at the first line.
        for expected in (0..4).rev() {
            assert_eq!(
                shell
                    .execute_checked(&NoopFs, ShellCommand::DiffPrevLine)
                    .unwrap(),
                CommandResult::Applied
            );
            assert_eq!(
                shell.diff_panel_ref().unwrap().selected_index(),
                Some(expected)
            );
        }
        assert_eq!(
            shell
                .execute_checked(&NoopFs, ShellCommand::DiffPrevLine)
                .unwrap(),
            CommandResult::Noop
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(0));
    }

    #[test]
    fn diff_key_bindings_route_through_input() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());

        // Ctrl+J next / Ctrl+K prev.
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 'j')).unwrap(),
            CommandResult::Applied
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(0));
        // Uppercase K is normalized to the same prev binding.
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 'K')).unwrap(),
            CommandResult::Noop
        );
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 'j')).unwrap(),
            CommandResult::Applied
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(1));
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 'k')).unwrap(),
            CommandResult::Applied
        );
        assert_eq!(shell.diff_panel_ref().unwrap().selected_index(), Some(0));

        // Ctrl+T with nothing selected in the tree is a Noop.
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 't')).unwrap(),
            CommandResult::Noop
        );

        // With a tree selection, Ctrl+T toggles that file's expansion.
        shell
            .app
            .file_tree
            .add("src/lib.rs", nextide_kernel::FileNodeKind::File);
        let _ = shell.tree_input_at(Point { x: 10, y: 0 });
        assert_eq!(
            shell.handle_input(&NoopFs, key(true, 't')).unwrap(),
            CommandResult::Applied
        );
        assert!(!shell
            .diff_panel_ref()
            .unwrap()
            .is_expanded(std::path::Path::new("src/lib.rs")));
    }

    #[test]
    fn diff_click_input_maps_to_toggle_command() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());
        assert_eq!(
            shell
                .handle_input(&NoopFs, UiInput::ClickDiffFile("docs/notes.md".into()))
                .unwrap(),
            CommandResult::Applied
        );
        assert!(!shell
            .diff_panel_ref()
            .unwrap()
            .is_expanded(std::path::Path::new("docs/notes.md")));
    }

    #[test]
    fn diff_commands_observe_the_typed_execution_path() {
        let mut shell = DesktopShell::new("NextIDE");
        shell.update_diff_panel(sample_diff());
        let result = shell.execute_observed(&NoopFs, ShellCommand::DiffNextLine);
        assert!(result.is_applied());
        assert_eq!(result.command_id(), "diff.next-line");
        assert_eq!(shell.events.pending("shell.command.executed"), 1);
        assert_eq!(
            shell.events.drain("shell.command.executed")[0].payload,
            "id=diff.next-line,outcome=applied"
        );

        // Without a panel the same command runs as a benign no-op.
        let mut shell = DesktopShell::new("NextIDE");
        let result = shell.execute_observed(&NoopFs, ShellCommand::DiffPrevLine);
        assert!(result.is_noop());
        assert_eq!(result.command_id(), "diff.prev-line");
        assert_eq!(
            shell.events.drain("shell.command.executed")[0].payload,
            "id=diff.prev-line,outcome=noop"
        );
    }
}

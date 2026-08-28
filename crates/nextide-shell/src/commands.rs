use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellCommand {
    OpenFile(PathBuf),
    CloseFile { path: PathBuf, force: bool },
    SaveActive,
    ScanWorkspace(PathBuf),
    ToggleDirectory(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandResult {
    Applied,
    Noop,
}

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

pub fn from_id(id: &str, path: impl Into<PathBuf>) -> Option<ShellCommand> {
    let path = path.into();
    match id {
        "file.open" => Some(ShellCommand::OpenFile(path)),
        "file.close" => Some(ShellCommand::CloseFile { path, force: false }),
        "file.save" => Some(ShellCommand::SaveActive),
        "workspace.scan" => Some(ShellCommand::ScanWorkspace(path)),
        "workspace.toggle-directory" => Some(ShellCommand::ToggleDirectory(path)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_stable_command_ids() {
        assert_eq!(from_id("file.save", "."), Some(ShellCommand::SaveActive));
        assert!(from_id("missing", ".").is_none());
    }
}

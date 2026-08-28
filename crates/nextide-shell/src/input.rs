use crate::{KeyChord, ShellCommand};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiInput {
    Key(KeyChord),
    ClickFile(PathBuf),
    ClickDirectory(PathBuf),
    ClickTab(PathBuf),
    Resize { width: u32, height: u32 },
    Close,
}

pub fn command_for_input(input: UiInput) -> Option<ShellCommand> {
    match input {
        UiInput::Key(chord) => crate::command_for(chord),
        UiInput::ClickFile(path) => Some(ShellCommand::OpenFile(path)),
        UiInput::ClickDirectory(path) => Some(ShellCommand::ToggleDirectory(path)),
        UiInput::ClickTab(path) => Some(ShellCommand::OpenFile(path)),
        UiInput::Resize { .. } | UiInput::Close => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_file_and_tab_clicks_to_open_commands() {
        let path = PathBuf::from("main.rs");
        assert_eq!(
            command_for_input(UiInput::ClickFile(path.clone())),
            Some(ShellCommand::OpenFile(path.clone()))
        );
        assert_eq!(
            command_for_input(UiInput::ClickTab(path.clone())),
            Some(ShellCommand::OpenFile(path))
        );
        assert_eq!(
            command_for_input(UiInput::ClickDirectory(PathBuf::from("src"))),
            Some(ShellCommand::ToggleDirectory(PathBuf::from("src")))
        );
        assert!(command_for_input(UiInput::Close).is_none());
    }
}

use crate::ShellCommand;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyChord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: char,
}

pub fn command_for(chord: KeyChord) -> Option<ShellCommand> {
    if chord.ctrl && !chord.shift && !chord.alt {
        return match chord.key.to_ascii_lowercase() {
            's' => Some(ShellCommand::SaveActive),
            'o' => Some(ShellCommand::OpenFile(PathBuf::new())),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_core_editor_shortcuts() {
        let save = command_for(KeyChord {
            ctrl: true,
            shift: false,
            alt: false,
            key: 's',
        });
        assert_eq!(save, Some(ShellCommand::SaveActive));
        assert_eq!(
            command_for(KeyChord {
                ctrl: false,
                shift: false,
                alt: false,
                key: 'x'
            }),
            None
        );
    }
}

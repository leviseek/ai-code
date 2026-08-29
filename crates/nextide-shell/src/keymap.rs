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
            // Diff panel navigation. The toggle command carries an empty path
            // so the shell can resolve it to the selected tree path.
            'j' => Some(ShellCommand::DiffNextLine),
            'k' => Some(ShellCommand::DiffPrevLine),
            't' => Some(ShellCommand::DiffToggleFile(PathBuf::new())),
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

    #[test]
    fn maps_diff_panel_navigation_shortcuts() {
        let next = command_for(KeyChord {
            ctrl: true,
            shift: false,
            alt: false,
            key: 'j',
        });
        assert_eq!(next, Some(ShellCommand::DiffNextLine));
        let prev = command_for(KeyChord {
            ctrl: true,
            shift: false,
            alt: false,
            key: 'K',
        });
        assert_eq!(prev, Some(ShellCommand::DiffPrevLine));
        let toggle = command_for(KeyChord {
            ctrl: true,
            shift: false,
            alt: false,
            key: 't',
        });
        assert_eq!(toggle, Some(ShellCommand::DiffToggleFile(PathBuf::new())));
    }
}

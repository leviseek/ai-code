use std::env;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalProfile {
    pub program: String,
    pub args: Vec<String>,
}

impl TerminalProfile {
    pub fn default_shell() -> Self {
        if cfg!(windows) {
            let program = env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
            Self {
                program,
                args: vec!["/K".into()],
            }
        } else {
            let program = env::var("SHELL").unwrap_or_else(|_| "sh".into());
            Self {
                program,
                args: Vec::new(),
            }
        }
    }

    pub fn powershell() -> Self {
        if cfg!(windows) {
            Self {
                program: "powershell.exe".into(),
                args: vec!["-NoLogo".into()],
            }
        } else {
            Self {
                program: "sh".into(),
                args: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_shell_has_a_program() {
        assert!(!TerminalProfile::default_shell().program.is_empty());
    }
    #[test]
    fn powershell_profile_is_deterministic_on_windows() {
        let profile = TerminalProfile::powershell();
        if cfg!(windows) {
            assert_eq!(profile.program, "powershell.exe");
        }
    }
}

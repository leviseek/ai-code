use crate::ShellCommand;

#[derive(Clone, Debug)]
pub struct CommandDescriptor {
    pub id: &'static str,
    pub title: &'static str,
    pub keywords: &'static [&'static str],
}

pub const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor {
        id: "file.open",
        title: "Open File",
        keywords: &["open", "file"],
    },
    CommandDescriptor {
        id: "file.save",
        title: "Save File",
        keywords: &["save", "write"],
    },
    CommandDescriptor {
        id: "file.close",
        title: "Close File",
        keywords: &["close", "tab"],
    },
    CommandDescriptor {
        id: "workspace.scan",
        title: "Scan Workspace",
        keywords: &["workspace", "refresh"],
    },
];

pub fn search(query: &str) -> Vec<&'static CommandDescriptor> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .filter(|command| {
            query.is_empty()
                || command.title.to_ascii_lowercase().contains(&query)
                || command
                    .keywords
                    .iter()
                    .any(|keyword| keyword.contains(&query))
        })
        .collect()
}

pub fn command_id(command: &ShellCommand) -> &'static str {
    match command {
        ShellCommand::OpenFile(_) => "file.open",
        ShellCommand::CloseFile { .. } => "file.close",
        ShellCommand::SaveActive => "file.save",
        ShellCommand::ScanWorkspace(_) => "workspace.scan",
        ShellCommand::ToggleDirectory(_) => "workspace.toggle-directory",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn searches_commands_by_title_and_keyword() {
        assert_eq!(search("save")[0].id, "file.save");
        assert_eq!(search("refresh")[0].id, "workspace.scan");
        assert_eq!(search("").len(), COMMANDS.len());
    }
}

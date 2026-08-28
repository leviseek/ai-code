use crate::ShellCommand;

#[derive(Clone, Debug)]
pub struct CommandDescriptor {
    pub id: &'static str,
    pub title: &'static str,
    pub keywords: &'static [&'static str],
}

/// A stable, UI-safe snapshot of a registered command's id, title, and search
/// keywords, decoupled from the static descriptor table so it can cross API
/// boundaries (e.g. command palette, UI rendering, or plugins) without
/// borrowing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSummary {
    pub id: String,
    pub title: String,
    pub keywords: Vec<String>,
}

impl From<&CommandDescriptor> for CommandSummary {
    fn from(descriptor: &CommandDescriptor) -> Self {
        Self {
            id: descriptor.id.to_owned(),
            title: descriptor.title.to_owned(),
            keywords: descriptor
                .keywords
                .iter()
                .map(|keyword| (*keyword).to_owned())
                .collect(),
        }
    }
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

/// Returns the ids of all registered commands in stable (declaration) order.
pub fn command_ids() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().map(|command| command.id)
}

/// Resolves a registered command descriptor by its stable id.
pub fn get_command(id: &str) -> Option<&'static CommandDescriptor> {
    COMMANDS.iter().find(|command| command.id == id)
}

/// Returns a snapshot projection of one command, or `None` when the id is not
/// registered.
pub fn command_summary(id: &str) -> Option<CommandSummary> {
    get_command(id).map(CommandSummary::from)
}

/// Returns snapshot projections of all registered commands in stable
/// (declaration) order.
pub fn command_summaries() -> Vec<CommandSummary> {
    COMMANDS.iter().map(CommandSummary::from).collect()
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

    #[test]
    fn lists_command_ids_in_stable_order() {
        let ids: Vec<&str> = command_ids().collect();
        assert_eq!(
            ids,
            ["file.open", "file.save", "file.close", "workspace.scan"]
        );
        assert_eq!(ids.len(), COMMANDS.len());
    }

    #[test]
    fn command_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for command in COMMANDS {
            assert!(
                seen.insert(command.id),
                "duplicate command id: {}",
                command.id
            );
        }
    }

    #[test]
    fn get_command_resolves_registered_commands() {
        let open = get_command("file.open").expect("file.open should be registered");
        assert_eq!(open.title, "Open File");
        assert_eq!(open.keywords, ["open", "file"]);
        assert!(get_command("missing").is_none());
    }

    #[test]
    fn command_summary_projects_metadata() {
        assert_eq!(
            command_summary("file.open"),
            Some(CommandSummary {
                id: "file.open".into(),
                title: "Open File".into(),
                keywords: vec!["open".into(), "file".into()],
            })
        );
        assert_eq!(command_summary("missing"), None);
    }

    #[test]
    fn command_summaries_cover_all_registered_commands_in_order() {
        let summaries = command_summaries();
        assert_eq!(summaries.len(), COMMANDS.len());
        let ids: Vec<&str> = command_ids().collect();
        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        assert!(summaries
            .iter()
            .all(|summary| !summary.id.is_empty() && !summary.title.is_empty()));
    }

    #[test]
    fn every_registered_command_id_round_trips_through_from_id() {
        for command in COMMANDS {
            assert!(
                crate::command_from_id(command.id, ".").is_some(),
                "command id {} should resolve to a ShellCommand",
                command.id
            );
        }
    }
}

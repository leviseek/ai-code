use std::fmt;
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

impl CommandResult {
    /// Returns true when the command changed shell state.
    pub fn is_applied(&self) -> bool {
        matches!(self, CommandResult::Applied)
    }

    /// Returns true when the command ran as a benign no-op.
    pub fn is_noop(&self) -> bool {
        matches!(self, CommandResult::Noop)
    }
}

/// Tri-state classification of a command execution, including failure.
/// A superset of [`CommandResult`] that UI and plugins can match on without
/// inspecting the error payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandOutcome {
    Applied,
    Noop,
    Failed,
}

impl CommandOutcome {
    /// Stable machine-readable string used in event payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            CommandOutcome::Applied => "applied",
            CommandOutcome::Noop => "noop",
            CommandOutcome::Failed => "failed",
        }
    }

    /// Returns true when the command execution failed.
    pub fn is_failed(self) -> bool {
        matches!(self, CommandOutcome::Failed)
    }
}

impl fmt::Display for CommandOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<CommandResult> for CommandOutcome {
    fn from(result: CommandResult) -> Self {
        match result {
            CommandResult::Applied => CommandOutcome::Applied,
            CommandResult::Noop => CommandOutcome::Noop,
        }
    }
}

/// Machine-readable classification of a command failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandErrorKind {
    /// The underlying filesystem or IO operation failed.
    Io,
    /// The command id is not a registered/known command.
    NotFound,
    /// The shell state did not permit the command to run.
    InvalidState,
    /// An unexpected, unstructured failure.
    Unknown,
}

impl CommandErrorKind {
    /// Stable machine-readable string used in event payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            CommandErrorKind::Io => "io",
            CommandErrorKind::NotFound => "not-found",
            CommandErrorKind::InvalidState => "invalid-state",
            CommandErrorKind::Unknown => "unknown",
        }
    }
}

/// Typed, structured description of a command execution failure. Carries no
/// borrows, so it can cross UI/plugin boundaries (events, RPC, snapshots).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The underlying filesystem or IO operation failed; the original error
    /// kind is preserved for round-tripping back to `std::io::Error`.
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The command id is not a registered/known command.
    NotFound { message: String },
    /// The shell state did not permit the command to run.
    InvalidState { message: String },
    /// An unexpected, unstructured failure.
    Unknown { message: String },
}

impl CommandError {
    /// Captures an IO error while keeping its error kind and message.
    pub fn from_io(error: std::io::Error) -> Self {
        CommandError::Io {
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    /// Builds a not-found error for an unknown command id.
    pub fn not_found(command_id: impl Into<String>) -> Self {
        CommandError::NotFound {
            message: format!("command not found: {}", command_id.into()),
        }
    }

    /// Builds an invalid-state error (e.g. no active file to save).
    pub fn invalid_state(message: impl Into<String>) -> Self {
        CommandError::InvalidState {
            message: message.into(),
        }
    }

    /// Builds an unstructured error for unexpected failures.
    pub fn unknown(message: impl Into<String>) -> Self {
        CommandError::Unknown {
            message: message.into(),
        }
    }

    /// Returns the machine-readable failure class.
    pub fn kind(&self) -> CommandErrorKind {
        match self {
            CommandError::Io { .. } => CommandErrorKind::Io,
            CommandError::NotFound { .. } => CommandErrorKind::NotFound,
            CommandError::InvalidState { .. } => CommandErrorKind::InvalidState,
            CommandError::Unknown { .. } => CommandErrorKind::Unknown,
        }
    }

    /// Returns the human-readable failure message (no kind prefix).
    pub fn message(&self) -> &str {
        match self {
            CommandError::Io { message, .. }
            | CommandError::NotFound { message }
            | CommandError::InvalidState { message }
            | CommandError::Unknown { message } => message,
        }
    }

    /// Reconstructs the closest `std::io::Error`; `Io` failures keep their
    /// original error kind.
    pub fn into_io_error(self) -> std::io::Error {
        match self {
            CommandError::Io { kind, message } => std::io::Error::new(kind, message),
            CommandError::NotFound { message }
            | CommandError::InvalidState { message }
            | CommandError::Unknown { message } => std::io::Error::other(message),
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CommandError {}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        CommandError::from_io(error)
    }
}

impl From<CommandError> for std::io::Error {
    fn from(error: CommandError) -> Self {
        error.into_io_error()
    }
}

/// A typed, cloneable record of one command execution, consumable by UI and
/// plugins: the stable command id, the tri-state outcome, and the structured
/// error when the execution failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandExecutionResult {
    command_id: String,
    outcome: CommandOutcome,
    error: Option<CommandError>,
}

impl CommandExecutionResult {
    /// Records a successful execution that changed shell state.
    pub fn applied(command_id: impl Into<String>) -> Self {
        Self {
            command_id: command_id.into(),
            outcome: CommandOutcome::Applied,
            error: None,
        }
    }

    /// Records an execution that ran as a benign no-op.
    pub fn noop(command_id: impl Into<String>) -> Self {
        Self {
            command_id: command_id.into(),
            outcome: CommandOutcome::Noop,
            error: None,
        }
    }

    /// Records a failed execution with a typed error.
    pub fn failed(command_id: impl Into<String>, error: CommandError) -> Self {
        Self {
            command_id: command_id.into(),
            outcome: CommandOutcome::Failed,
            error: Some(error),
        }
    }

    /// Builds a result from the legacy `std::io::Result<CommandResult>` shape
    /// returned by [`execute`]-style APIs, converting any IO error into a
    /// typed [`CommandError`].
    pub fn from_io_result(
        command_id: impl Into<String>,
        result: std::io::Result<CommandResult>,
    ) -> Self {
        let command_id = command_id.into();
        match result {
            Ok(CommandResult::Applied) => Self::applied(command_id),
            Ok(CommandResult::Noop) => Self::noop(command_id),
            Err(error) => Self::failed(command_id, CommandError::from_io(error)),
        }
    }

    /// Returns the stable id of the executed command.
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    /// Returns the tri-state execution outcome.
    pub fn outcome(&self) -> CommandOutcome {
        self.outcome
    }

    /// Returns the structured error when the execution failed.
    pub fn error(&self) -> Option<&CommandError> {
        self.error.as_ref()
    }

    /// Returns true when the command changed shell state.
    pub fn is_applied(&self) -> bool {
        self.outcome == CommandOutcome::Applied
    }

    /// Returns true when the command ran as a benign no-op.
    pub fn is_noop(&self) -> bool {
        self.outcome == CommandOutcome::Noop
    }

    /// Returns true when the command execution failed.
    pub fn is_failed(&self) -> bool {
        self.outcome.is_failed()
    }

    /// Returns true when the execution did not fail (applied or no-op).
    pub fn is_success(&self) -> bool {
        !self.is_failed()
    }

    /// Returns a human-readable message: the error message on failure, or the
    /// outcome string otherwise.
    pub fn message(&self) -> String {
        match (&self.outcome, &self.error) {
            (CommandOutcome::Failed, Some(error)) => error.to_string(),
            (CommandOutcome::Failed, None) => "command failed".to_owned(),
            _ => self.outcome.to_string(),
        }
    }

    /// Returns a stable, parseable event-bus payload of the form
    /// `id=<id>,outcome=<outcome>` or, on failure,
    /// `id=<id>,outcome=failed,error=<kind>,message=<message>`.
    pub fn to_event_payload(&self) -> String {
        match (&self.outcome, &self.error) {
            (CommandOutcome::Failed, Some(error)) => format!(
                "id={},outcome=failed,error={},message={}",
                self.command_id,
                error.kind().as_str(),
                error.message()
            ),
            _ => format!("id={},outcome={}", self.command_id, self.outcome.as_str()),
        }
    }

    /// Converts this observable record back to the legacy
    /// `std::io::Result<CommandResult>` shape.
    pub fn into_io_result(self) -> std::io::Result<CommandResult> {
        match (self.outcome, self.error) {
            (CommandOutcome::Applied, _) => Ok(CommandResult::Applied),
            (CommandOutcome::Noop, _) => Ok(CommandResult::Noop),
            (CommandOutcome::Failed, error) => Err(error.map_or_else(
                || std::io::Error::other("command failed"),
                CommandError::into_io_error,
            )),
        }
    }
}

impl fmt::Display for CommandExecutionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.outcome, &self.error) {
            (CommandOutcome::Failed, Some(error)) => {
                write!(f, "{}: failed: {}", self.command_id, error)
            }
            _ => write!(f, "{}: {}", self.command_id, self.outcome),
        }
    }
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

    #[test]
    fn command_result_predicates_classify_outcomes() {
        assert!(CommandResult::Applied.is_applied());
        assert!(!CommandResult::Applied.is_noop());
        assert!(CommandResult::Noop.is_noop());
        assert!(!CommandResult::Noop.is_applied());
    }

    #[test]
    fn command_outcome_maps_from_command_result() {
        assert_eq!(
            CommandOutcome::from(CommandResult::Applied),
            CommandOutcome::Applied
        );
        assert_eq!(
            CommandOutcome::from(CommandResult::Noop),
            CommandOutcome::Noop
        );
        assert_eq!(CommandOutcome::Applied.as_str(), "applied");
        assert_eq!(CommandOutcome::Noop.as_str(), "noop");
        assert_eq!(CommandOutcome::Failed.as_str(), "failed");
        assert!(CommandOutcome::Failed.is_failed());
        assert!(!CommandOutcome::Applied.is_failed());
    }

    #[test]
    fn command_error_classifies_kinds_and_messages() {
        let io = CommandError::from_io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing file",
        ));
        assert_eq!(io.kind(), CommandErrorKind::Io);
        assert_eq!(io.message(), "missing file");

        let not_found = CommandError::not_found("file.open");
        assert_eq!(not_found.kind(), CommandErrorKind::NotFound);
        assert_eq!(not_found.message(), "command not found: file.open");

        let invalid = CommandError::invalid_state("no active file");
        assert_eq!(invalid.kind(), CommandErrorKind::InvalidState);
        assert_eq!(invalid.message(), "no active file");

        let unknown = CommandError::unknown("boom");
        assert_eq!(unknown.kind(), CommandErrorKind::Unknown);
        assert_eq!(unknown.message(), "boom");
    }

    #[test]
    fn command_error_round_trips_through_io_error() {
        let original = std::io::Error::new(std::io::ErrorKind::WouldBlock, "dirty file");
        let restored = CommandError::from_io(original).into_io_error();
        assert_eq!(restored.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(restored.to_string(), "dirty file");
        let restored: std::io::Error = CommandError::invalid_state("no active file").into();
        assert_eq!(restored.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn command_error_kind_strings_are_stable() {
        assert_eq!(CommandErrorKind::Io.as_str(), "io");
        assert_eq!(CommandErrorKind::NotFound.as_str(), "not-found");
        assert_eq!(CommandErrorKind::InvalidState.as_str(), "invalid-state");
        assert_eq!(CommandErrorKind::Unknown.as_str(), "unknown");
    }

    #[test]
    fn command_execution_result_queries() {
        let applied = CommandExecutionResult::applied("file.save");
        assert_eq!(applied.command_id(), "file.save");
        assert_eq!(applied.outcome(), CommandOutcome::Applied);
        assert!(applied.is_applied());
        assert!(applied.is_success());
        assert!(!applied.is_failed());
        assert!(applied.error().is_none());
        assert_eq!(applied.message(), "applied");

        let noop = CommandExecutionResult::noop("file.close");
        assert_eq!(noop.outcome(), CommandOutcome::Noop);
        assert!(noop.is_noop());
        assert!(noop.is_success());

        let failed =
            CommandExecutionResult::failed("file.open", CommandError::invalid_state("no file"));
        assert!(failed.is_failed());
        assert!(!failed.is_success());
        assert_eq!(
            failed.error().unwrap().kind(),
            CommandErrorKind::InvalidState
        );
        assert_eq!(failed.message(), "no file");
    }

    #[test]
    fn command_execution_result_from_io_result() {
        assert_eq!(
            CommandExecutionResult::from_io_result("file.save", Ok(CommandResult::Applied)),
            CommandExecutionResult::applied("file.save")
        );
        assert_eq!(
            CommandExecutionResult::from_io_result("file.close", Ok(CommandResult::Noop)),
            CommandExecutionResult::noop("file.close")
        );
        let failed = CommandExecutionResult::from_io_result(
            "file.open",
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "dirty")),
        );
        assert!(failed.is_failed());
        assert_eq!(failed.error().unwrap().kind(), CommandErrorKind::Io);
        assert_eq!(failed.error().unwrap().message(), "dirty");
    }

    #[test]
    fn command_execution_result_into_io_result() {
        assert_eq!(
            CommandExecutionResult::applied("file.save")
                .into_io_result()
                .unwrap(),
            CommandResult::Applied
        );
        assert_eq!(
            CommandExecutionResult::noop("file.close")
                .into_io_result()
                .unwrap(),
            CommandResult::Noop
        );
        let err = CommandExecutionResult::failed(
            "file.open",
            CommandError::from_io(std::io::Error::new(std::io::ErrorKind::WouldBlock, "dirty")),
        )
        .into_io_result()
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn command_execution_payload_is_stable() {
        assert_eq!(
            CommandExecutionResult::applied("file.save").to_event_payload(),
            "id=file.save,outcome=applied"
        );
        assert_eq!(
            CommandExecutionResult::noop("file.close").to_event_payload(),
            "id=file.close,outcome=noop"
        );
        let failed = CommandExecutionResult::failed(
            "file.open",
            CommandError::from_io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        );
        assert_eq!(
            failed.to_event_payload(),
            "id=file.open,outcome=failed,error=io,message=missing"
        );
    }

    #[test]
    fn command_execution_result_displays_command_and_outcome() {
        assert_eq!(
            CommandExecutionResult::applied("file.save").to_string(),
            "file.save: applied"
        );
        assert_eq!(
            CommandExecutionResult::failed("file.open", CommandError::unknown("boom")).to_string(),
            "file.open: failed: boom"
        );
    }
}

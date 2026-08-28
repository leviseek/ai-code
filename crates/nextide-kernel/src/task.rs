use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskResult {
    pub state: TaskState,
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSpec {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TaskError {
    EmptyId,
    EmptyProgram,
    InvalidTransition,
    DuplicateId,
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task error: {:?}", self)
    }
}
impl std::error::Error for TaskError {}

#[derive(Debug)]
pub struct Task {
    pub spec: TaskSpec,
    pub state: TaskState,
    output: String,
}

impl Task {
    pub fn new(spec: TaskSpec) -> Result<Self, TaskError> {
        if spec.id.trim().is_empty() {
            return Err(TaskError::EmptyId);
        }
        if spec.program.trim().is_empty() {
            return Err(TaskError::EmptyProgram);
        }
        Ok(Self {
            spec,
            state: TaskState::Pending,
            output: String::new(),
        })
    }

    pub fn start(&mut self) -> Result<(), TaskError> {
        if self.state != TaskState::Pending {
            return Err(TaskError::InvalidTransition);
        }
        self.state = TaskState::Running;
        Ok(())
    }

    pub fn append_output(&mut self, chunk: &str) -> Result<(), TaskError> {
        if self.state != TaskState::Running {
            return Err(TaskError::InvalidTransition);
        }
        self.output.push_str(chunk);
        Ok(())
    }

    /// The output accumulated so far. Kept after a terminal transition by
    /// [`Task::finish_retain_output`] / [`Task::timeout_retain_output`];
    /// drained by [`Task::finish`] / [`Task::timeout`].
    pub fn output(&self) -> &str {
        &self.output
    }

    pub fn finish(&mut self, exit_code: i32) -> Result<TaskResult, TaskError> {
        if self.state != TaskState::Running {
            return Err(TaskError::InvalidTransition);
        }
        self.state = if exit_code == 0 {
            TaskState::Succeeded
        } else {
            TaskState::Failed
        };
        Ok(TaskResult {
            state: self.state,
            exit_code: Some(exit_code),
            output: std::mem::take(&mut self.output),
        })
    }

    /// Like [`Task::finish`], but leaves the accumulated output in the record
    /// instead of draining it into the returned result.
    ///
    /// The async worker uses this so the manager's task record keeps the
    /// streamed output after completion — the sync run methods hand the
    /// drained `TaskResult` to their caller, but the async caller observes
    /// output through the handle, so the record must not lose it.
    pub fn finish_retain_output(&mut self, exit_code: i32) -> Result<(), TaskError> {
        if self.state != TaskState::Running {
            return Err(TaskError::InvalidTransition);
        }
        self.state = if exit_code == 0 {
            TaskState::Succeeded
        } else {
            TaskState::Failed
        };
        Ok(())
    }

    pub fn timeout(&mut self) -> Result<TaskResult, TaskError> {
        if self.state != TaskState::Running {
            return Err(TaskError::InvalidTransition);
        }
        self.state = TaskState::TimedOut;
        Ok(TaskResult {
            state: self.state,
            exit_code: None,
            output: std::mem::take(&mut self.output),
        })
    }

    /// Like [`Task::timeout`], but leaves the accumulated output in the record
    /// instead of draining it; see [`Task::finish_retain_output`].
    pub fn timeout_retain_output(&mut self) -> Result<(), TaskError> {
        if self.state != TaskState::Running {
            return Err(TaskError::InvalidTransition);
        }
        self.state = TaskState::TimedOut;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), TaskError> {
        if self.state != TaskState::Running && self.state != TaskState::Pending {
            return Err(TaskError::InvalidTransition);
        }
        self.state = TaskState::Cancelled;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_has_explicit_lifecycle() {
        let spec = TaskSpec {
            id: "build".into(),
            program: "cargo".into(),
            args: vec!["build".into()],
        };
        let mut task = Task::new(spec).unwrap();
        task.start().unwrap();
        task.append_output("compiled\n").unwrap();
        let result = task.finish(0).unwrap();
        assert_eq!(result.state, TaskState::Succeeded);
        assert_eq!(result.output, "compiled\n");
        assert_eq!(task.cancel(), Err(TaskError::InvalidTransition));
    }

    #[test]
    fn terminal_transitions_can_retain_output() {
        let spec = TaskSpec {
            id: "keep".into(),
            program: "cargo".into(),
            args: vec![],
        };
        let mut task = Task::new(spec.clone()).unwrap();
        task.start().unwrap();
        task.append_output("compiled\n").unwrap();
        task.finish_retain_output(0).unwrap();
        assert_eq!(task.state, TaskState::Succeeded);
        // The output survives the transition; finish() on a terminal task is
        // rejected exactly like the draining variant.
        assert_eq!(task.output(), "compiled\n");
        assert_eq!(task.finish(0), Err(TaskError::InvalidTransition));
        assert_eq!(
            task.finish_retain_output(0),
            Err(TaskError::InvalidTransition)
        );

        let mut task = Task::new(spec.clone()).unwrap();
        task.start().unwrap();
        task.append_output("slow\n").unwrap();
        task.timeout_retain_output().unwrap();
        assert_eq!(task.state, TaskState::TimedOut);
        assert_eq!(task.output(), "slow\n");
        assert_eq!(task.timeout(), Err(TaskError::InvalidTransition));
    }
}

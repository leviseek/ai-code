use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
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
}

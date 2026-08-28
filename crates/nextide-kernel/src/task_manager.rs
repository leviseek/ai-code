use crate::run_capture;
use crate::{Event, EventBus, Task, TaskError, TaskResult, TaskSpec};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

#[derive(Default, Debug)]
pub struct TaskManager {
    tasks: BTreeMap<String, Task>,
}

impl TaskManager {
    pub fn add(&mut self, spec: TaskSpec) -> Result<(), TaskError> {
        let id = spec.id.clone();
        if self.tasks.contains_key(&id) {
            return Err(TaskError::DuplicateId);
        }
        self.tasks.insert(id, Task::new(spec)?);
        Ok(())
    }
    pub fn start(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        let task = self.tasks.get_mut(id).ok_or(TaskError::InvalidTransition)?;
        task.start()?;
        events.publish(Event::new("task.started", id));
        Ok(())
    }
    pub fn append_output(&mut self, id: &str, chunk: &str) -> Result<(), TaskError> {
        self.tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .append_output(chunk)
    }
    pub fn cancel(&mut self, id: &str, events: &mut EventBus) -> Result<(), TaskError> {
        self.tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .cancel()?;
        events.publish(Event::new("task.cancelled", id));
        Ok(())
    }

    pub fn run_capture(
        &mut self,
        id: &str,
        working_directory: &Path,
        events: &mut EventBus,
    ) -> io::Result<TaskResult> {
        self.start(id, events)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let output = run_capture(&spec.program, &spec.args, working_directory)?;
        let combined = format!("{}{}", output.stdout, output.stderr);
        self.append_output(id, &combined)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        self.finish(id, output.exit_code.unwrap_or(-1), events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    pub fn finish(
        &mut self,
        id: &str,
        code: i32,
        events: &mut EventBus,
    ) -> Result<TaskResult, TaskError> {
        let result = self
            .tasks
            .get_mut(id)
            .ok_or(TaskError::InvalidTransition)?
            .finish(code)?;
        events.publish(Event::new("task.finished", format!("{id}:{}", code)));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_runs_real_process_and_captures_output() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo task".into()])
        } else {
            ("sh", vec!["-c".into(), "printf task".into()])
        };
        manager
            .add(TaskSpec {
                id: "echo".into(),
                program: program.into(),
                args,
            })
            .unwrap();
        let result = manager
            .run_capture("echo", Path::new("."), &mut events)
            .unwrap();
        assert_eq!(result.state, crate::TaskState::Succeeded);
        assert!(result.output.contains("task"));
    }

    #[test]
    fn manager_emits_task_lifecycle_events() {
        let mut manager = TaskManager::default();
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "build".into(),
                program: "cargo".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("build", &mut events).unwrap();
        manager.append_output("build", "ok").unwrap();
        let result = manager.finish("build", 0, &mut events).unwrap();
        assert_eq!(result.output, "ok");
        assert_eq!(events.drain("task.started")[0].payload, "build");
        assert_eq!(events.drain("task.finished")[0].payload, "build:0");
    }
}

use crate::{run_capture, run_capture_timeout};
use crate::{Event, EventBus, Task, TaskError, TaskResult, TaskSpec};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: String,
    pub state: crate::TaskState,
    pub program: String,
}

#[derive(Default, Debug)]
pub struct TaskManager {
    tasks: BTreeMap<String, Task>,
}

impl TaskManager {
    pub fn clear_finished_with_events(&mut self, events: &mut EventBus) -> usize {
        let cleared = self.clear_finished();
        if cleared > 0 {
            events.publish(Event::new("task.cleaned", cleared.to_string()));
        }
        cleared
    }

    pub fn clear_finished(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|_, task| {
            matches!(
                task.state,
                crate::TaskState::Pending | crate::TaskState::Running
            )
        });
        before - self.tasks.len()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), TaskError> {
        match self.tasks.get(id).map(|task| task.state) {
            Some(crate::TaskState::Pending) => {
                self.tasks.remove(id);
                Ok(())
            }
            Some(_) | None => Err(TaskError::InvalidTransition),
        }
    }

    pub fn summaries(&self) -> Vec<TaskSummary> {
        self.tasks
            .iter()
            .map(|(id, task)| TaskSummary {
                id: id.clone(),
                state: task.state,
                program: task.spec.program.clone(),
            })
            .collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.tasks.contains_key(id)
    }

    pub fn state(&self, id: &str) -> Option<crate::TaskState> {
        self.tasks.get(id).map(|task| task.state)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.tasks.keys().map(String::as_str)
    }

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
    pub fn append_output_with_events(
        &mut self,
        id: &str,
        chunk: &str,
        events: &mut EventBus,
    ) -> Result<(), TaskError> {
        self.append_output(id, chunk)?;
        events.publish(Event::new("task.output", format!("{id}:{chunk}")));
        Ok(())
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
        self.append_output_with_events(id, &combined, events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        self.finish(id, output.exit_code.unwrap_or(-1), events)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    pub fn run_capture_timeout(
        &mut self,
        id: &str,
        working_directory: &Path,
        timeout: std::time::Duration,
        events: &mut EventBus,
    ) -> io::Result<TaskResult> {
        self.start(id, events)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let spec = &self
            .tasks
            .get(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task not found"))?
            .spec;
        let output = run_capture_timeout(&spec.program, &spec.args, working_directory, timeout)?;
        let combined = format!("{}{}", output.stdout, output.stderr);
        self.append_output_with_events(id, &combined, events)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if output.exit_code.is_none() {
            let result = self
                .tasks
                .get_mut(id)
                .ok_or(TaskError::InvalidTransition)
                .and_then(|task| task.timeout())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            events.publish(Event::new("task.timeout", id));
            events.publish(Event::new("task.finished", format!("{id}:timeout")));
            Ok(result)
        } else {
            self.finish(id, output.exit_code.unwrap(), events)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
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
        if code != 0 {
            events.publish(Event::new("task.failed", format!("{id}:{code}")));
        }
        events.publish(Event::new("task.finished", format!("{id}:{code}")));
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
    fn manager_exposes_task_state_and_ids() {
        let mut manager = TaskManager::default();
        manager
            .add(TaskSpec {
                id: "build".into(),
                program: "echo".into(),
                args: vec![],
            })
            .unwrap();
        assert!(manager.contains("build"));
        assert!(!manager.contains("missing"));
        assert_eq!(manager.state("build"), Some(crate::TaskState::Pending));
        assert_eq!(manager.state("missing"), None);
        assert_eq!(manager.ids().collect::<Vec<_>>(), vec!["build"]);
        manager.remove("build").unwrap();
        assert!(manager.ids().next().is_none());
        assert_eq!(manager.remove("missing"), Err(TaskError::InvalidTransition));
        assert_eq!(manager.clear_finished(), 0);
        let mut events = EventBus::default();
        manager
            .add(TaskSpec {
                id: "done".into(),
                program: "echo".into(),
                args: vec![],
            })
            .unwrap();
        manager.start("done", &mut events).unwrap();
        manager.finish("done", 0, &mut events).unwrap();
        assert_eq!(manager.clear_finished_with_events(&mut events), 1);
        assert_eq!(manager.state("done"), None);
        assert_eq!(events.drain("task.cleaned")[0].payload, "1");
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

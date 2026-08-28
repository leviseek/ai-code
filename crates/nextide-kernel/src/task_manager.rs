use crate::{Event, EventBus, Task, TaskError, TaskResult, TaskSpec};
use std::collections::BTreeMap;

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

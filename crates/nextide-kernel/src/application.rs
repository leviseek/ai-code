use crate::{CapabilityRegistry, EditorKernel, EventBus, WorkspaceState};

/// Platform-independent application state shared by desktop shells.
#[derive(Debug)]
pub struct ApplicationContext {
    pub editor: EditorKernel,
    pub capabilities: CapabilityRegistry,
    pub events: EventBus,
    pub workspace: WorkspaceState,
}

impl ApplicationContext {
    pub fn new(initial_text: impl Into<String>) -> Self {
        Self {
            editor: EditorKernel::new(initial_text),
            capabilities: CapabilityRegistry::default(),
            events: EventBus::default(),
            workspace: WorkspaceState::new("."),
        }
    }

    pub fn bootstrap(initial_text: impl Into<String>) -> Self {
        let mut context = Self::new(initial_text);
        for (id, permission) in [
            ("workspace.read", "workspace.read"),
            ("workspace.write", "workspace.write"),
            ("terminal.execute", "terminal.execute"),
        ] {
            context
                .capabilities
                .register(crate::CapabilityDescriptor::new(id, "1.0").with_permission(permission))
                .expect("built-in capability ids are unique");
        }
        context
            .events
            .publish(crate::Event::new("application.ready", "kernel"));
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityDescriptor, EditTransaction, TextEdit};

    #[test]
    fn bootstrap_registers_core_capabilities() {
        let context = ApplicationContext::bootstrap("");
        assert!(context.capabilities.contains("workspace.read"));
        assert!(context.capabilities.contains("terminal.execute"));
        assert_eq!(context.events.pending("application.ready"), 1);
    }

    #[test]
    fn context_connects_editor_events_and_capabilities() {
        let mut context = ApplicationContext::new("hello");
        context
            .capabilities
            .register(CapabilityDescriptor::new("search", "1.0"))
            .unwrap();
        context
            .editor
            .apply_with_events(
                EditTransaction::new("edit", 0, vec![TextEdit::replace(0, 5, "world")]),
                &mut context.events,
            )
            .unwrap();
        assert_eq!(context.editor.document().text(), "world");
        assert_eq!(context.events.pending("document.changed"), 1);
        assert!(context.capabilities.contains("search"));
    }
}

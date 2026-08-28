use crate::{CapabilityRegistry, EditorKernel, EventBus};

/// Platform-independent application state shared by desktop shells.
#[derive(Debug)]
pub struct ApplicationContext {
    pub editor: EditorKernel,
    pub capabilities: CapabilityRegistry,
    pub events: EventBus,
}

impl ApplicationContext {
    pub fn new(initial_text: impl Into<String>) -> Self {
        Self {
            editor: EditorKernel::new(initial_text),
            capabilities: CapabilityRegistry::default(),
            events: EventBus::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityDescriptor, EditTransaction, TextEdit};

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

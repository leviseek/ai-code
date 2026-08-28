use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub version: String,
    pub permissions: Vec<String>,
}

impl CapabilityDescriptor {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            permissions: Vec::new(),
        }
    }

    pub fn with_permission(mut self, permission: impl Into<String>) -> Self {
        self.permissions.push(permission.into());
        self
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    EmptyId,
    Duplicate(String),
    NotFound(String),
    PermissionDenied {
        capability: String,
        permission: String,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => write!(f, "capability id cannot be empty"),
            Self::Duplicate(id) => write!(f, "capability already registered: {id}"),
            Self::NotFound(id) => write!(f, "capability not found: {id}"),
            Self::PermissionDenied {
                capability,
                permission,
            } => write!(f, "permission denied: {permission} for {capability}"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Default, Debug)]
pub struct CapabilityRegistry {
    entries: BTreeMap<String, CapabilityDescriptor>,
}

impl CapabilityRegistry {
    pub fn register(&mut self, descriptor: CapabilityDescriptor) -> Result<(), RegistryError> {
        if descriptor.id.trim().is_empty() {
            return Err(RegistryError::EmptyId);
        }
        if self.entries.contains_key(&descriptor.id) {
            return Err(RegistryError::Duplicate(descriptor.id));
        }
        self.entries.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    pub fn authorize(&self, id: &str, granted: &[String]) -> Result<(), RegistryError> {
        let descriptor = self.get(id)?;
        if let Some(permission) = descriptor
            .permissions
            .iter()
            .find(|permission| !granted.contains(permission))
        {
            return Err(RegistryError::PermissionDenied {
                capability: id.to_owned(),
                permission: permission.clone(),
            });
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&CapabilityDescriptor, RegistryError> {
        self.entries
            .get(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_owned()))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_resolves_capability() {
        let mut registry = CapabilityRegistry::default();
        registry
            .register(
                CapabilityDescriptor::new("workspace.read", "1.0")
                    .with_permission("workspace.read"),
            )
            .unwrap();
        assert_eq!(registry.get("workspace.read").unwrap().version, "1.0");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn enforces_declared_permissions() {
        let mut registry = CapabilityRegistry::default();
        registry
            .register(
                CapabilityDescriptor::new("terminal", "1.0").with_permission("terminal.execute"),
            )
            .unwrap();
        let no_permissions = Vec::new();
        assert_eq!(
            registry.authorize("terminal", &no_permissions),
            Err(RegistryError::PermissionDenied {
                capability: "terminal".into(),
                permission: "terminal.execute".into()
            })
        );
        let granted = vec!["terminal.execute".into()];
        assert!(registry.authorize("terminal", &granted).is_ok());
    }

    #[test]
    fn rejects_duplicate_and_missing_capabilities() {
        let mut registry = CapabilityRegistry::default();
        registry
            .register(CapabilityDescriptor::new("git", "1.0"))
            .unwrap();
        assert_eq!(
            registry.register(CapabilityDescriptor::new("git", "2.0")),
            Err(RegistryError::Duplicate("git".into()))
        );
        assert_eq!(
            registry.get("lsp"),
            Err(RegistryError::NotFound("lsp".into()))
        );
    }
}

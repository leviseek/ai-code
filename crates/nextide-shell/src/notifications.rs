#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub level: NotificationLevel,
    pub message: String,
}

#[derive(Default, Debug)]
pub struct NotificationCenter {
    items: Vec<Notification>,
}

impl NotificationCenter {
    pub fn push(&mut self, level: NotificationLevel, message: impl Into<String>) {
        self.items.push(Notification {
            level,
            message: message.into(),
        });
    }
    pub fn dismiss(&mut self, index: usize) -> Option<Notification> {
        if index < self.items.len() {
            Some(self.items.remove(index))
        } else {
            None
        }
    }
    pub fn items(&self) -> &[Notification] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_center_preserves_level_and_dismisses() {
        let mut center = NotificationCenter::default();
        center.push(NotificationLevel::Error, "failed");
        assert_eq!(center.items()[0].level, NotificationLevel::Error);
        assert_eq!(center.dismiss(0).unwrap().message, "failed");
        assert!(center.items().is_empty());
    }
}

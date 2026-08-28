use nextide_kernel::ApplicationContext;

#[derive(Debug)]
pub struct DesktopShell {
    pub app: ApplicationContext,
    pub title: String,
}

impl DesktopShell {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            app: ApplicationContext::bootstrap(""),
            title: title.into(),
        }
    }

    pub fn start(&self) -> String {
        format!(
            "{} ready ({} capabilities)",
            self.title,
            self.app.capabilities.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_starts_with_bootstrapped_application() {
        let shell = DesktopShell::new("NextIDE");
        assert_eq!(shell.start(), "NextIDE ready (3 capabilities)");
    }
}

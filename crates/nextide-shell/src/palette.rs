use crate::{search_commands, CommandDescriptor};

#[derive(Clone, Debug, Default)]
pub struct CommandPalette {
    query: String,
    selected: usize,
    open: bool,
}

impl CommandPalette {
    pub fn open(&mut self) {
        self.open = true;
        self.selected = 0;
    }
    pub fn close(&mut self) {
        self.open = false;
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn results(&self) -> Vec<&'static CommandDescriptor> {
        search_commands(&self.query)
    }
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.results().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }
    pub fn selected(&self) -> Option<&'static CommandDescriptor> {
        self.results().get(self.selected).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_filters_and_wraps_selection() {
        let mut palette = CommandPalette::default();
        palette.open();
        palette.set_query("file");
        assert_eq!(palette.results().len(), 3);
        palette.move_selection(-1);
        assert_eq!(palette.selected().unwrap().id, "file.close");
    }
}

use super::draw_picker_list;

const COMMANDS: &[&str] = &[
    "/add",
    "/drop",
    "/drop-all",
    "/init",
    "/memory",
    "/model",
    "/models",
    "/models-add",
    "/provider",
    "/sessions",
    "/reasoning",
    "/thinking",
    "/mode",
    "/mcp",
    "/toggle",
    "/compress",
    "/compact",
    "/loop",
    "/prompt",
    "/theme",
    "/history",
    "/regen-prompts",
    "/regen-themes",
    "/editsys",
    "/quit",
    "/exit",
    "/clear",
    "/new",
    "/undo",
    "/retry",
    "/help",
    "/welcome",
    "/tutorial",
    "/worktree",
    "/wt-merge",
    "/wt-exit",
    "/btw",
    "/queue",
];

pub struct ListPicker {
    pub active: bool,
    pub query: String,
    pub cursor: usize,
    pub matches: Vec<String>,
    pub selected: usize,
    items: Vec<String>,
    monochrome: bool,
}

impl ListPicker {
    pub fn new() -> Self {
        ListPicker {
            active: false,
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            selected: 0,
            items: Vec::new(),
            monochrome: false,
        }
    }

    pub fn with_static_commands() -> Self {
        let mut picker = ListPicker::new();
        picker.items = COMMANDS.iter().map(|s| s.to_string()).collect();
        picker
    }

    pub fn set_monochrome(&mut self, monochrome: bool) {
        self.monochrome = monochrome;
    }

    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items;
    }

    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.cursor = 0;
        self.matches.clear();
        self.selected = 0;
        self.filter();
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    pub fn char_input(&mut self, c: char) {
        let byte_pos = self
            .query
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());
        self.query.insert(byte_pos, c);
        self.cursor += 1;
        self.filter();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 && !self.query.is_empty() {
            self.cursor -= 1;
            let byte_pos = self
                .query
                .char_indices()
                .nth(self.cursor)
                .map(|(i, _)| i)
                .unwrap_or(self.query.len());
            self.query.remove(byte_pos);
            self.filter();
        }
    }

    fn filter(&mut self) {
        let query_lower = self.query.to_lowercase();
        self.matches = self
            .items
            .iter()
            .filter(|name| name.to_lowercase().contains(&query_lower))
            .take(50)
            .cloned()
            .collect();
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected == 0 {
                self.matches.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|s| s.as_str())
    }

    pub fn draw(&self, empty_message: Option<&str>) -> std::io::Result<()> {
        if !self.active {
            return Ok(());
        }
        draw_picker_list(
            &self.matches,
            self.selected,
            self.monochrome,
            empty_message,
            4,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_picker_filter() {
        let mut picker = ListPicker::new();
        picker.set_items(vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ]);
        picker.activate();
        assert_eq!(picker.matches.len(), 3);

        picker.char_input('a');
        assert_eq!(picker.matches, vec!["alpha", "beta", "gamma"]);

        picker.char_input('l');
        assert_eq!(picker.matches, vec!["alpha"]);
    }

    #[test]
    fn test_list_picker_navigation() {
        let mut picker = ListPicker::new();
        picker.set_items(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        picker.activate();
        assert_eq!(picker.selected, 0);

        picker.select_next();
        assert_eq!(picker.selected, 1);

        picker.select_prev();
        assert_eq!(picker.selected, 0);

        picker.select_prev();
        assert_eq!(picker.selected, 2);
    }

    #[test]
    fn test_list_picker_backspace_and_char_unicode() {
        let mut picker = ListPicker::new();
        picker.set_items(vec!["test".to_string()]);

        picker.char_input('é');
        assert_eq!(picker.query, "é");
        assert_eq!(picker.cursor, 1);

        picker.char_input('ñ');
        assert_eq!(picker.query, "éñ");
        assert_eq!(picker.cursor, 2);

        picker.backspace();
        assert_eq!(picker.query, "é");
        assert_eq!(picker.cursor, 1);

        picker.backspace();
        assert_eq!(picker.query, "");
        assert_eq!(picker.cursor, 0);

        picker.backspace();
        assert_eq!(picker.query, "");
        assert_eq!(picker.cursor, 0);
    }

    #[test]
    fn test_list_picker_reactivate_resets_state() {
        let mut picker = ListPicker::new();
        picker.set_items(vec!["a".to_string(), "b".to_string()]);
        picker.char_input('a');
        picker.char_input('b');
        assert_eq!(picker.query, "ab");

        picker.deactivate();
        assert!(!picker.active);

        picker.activate();
        assert!(picker.active);
        assert_eq!(picker.query, "");
        assert_eq!(picker.cursor, 0);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn test_static_commands_prepopulated() {
        let mut picker = ListPicker::with_static_commands();
        picker.activate();
        assert!(picker.matches.len() > 5);

        picker.char_input('m');
        picker.char_input('o');
        picker.char_input('d');
        assert!(picker.matches.contains(&"/model".to_string()));
    }
}

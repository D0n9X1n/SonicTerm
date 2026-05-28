//! Searchable keyboard-shortcuts cheat sheet state and filtering.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheatsheetState {
    pub query: String,
    pub selected_idx: usize,
}

impl CheatsheetState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.selected_idx = 0;
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected_idx = 0;
    }

    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected_idx = 0;
        }
    }

    pub fn move_selection_down(&mut self, len: usize) {
        if len == 0 {
            self.selected_idx = 0;
        } else {
            self.selected_idx = (self.selected_idx + 1) % len;
        }
    }

    pub fn move_selection_up(&mut self, len: usize) {
        if len == 0 {
            self.selected_idx = 0;
        } else if self.selected_idx == 0 {
            self.selected_idx = len - 1;
        } else {
            self.selected_idx -= 1;
        }
    }
}

#[must_use]
pub fn filter_indices(bindings: &[(String, String)], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..bindings.len()).collect();
    }

    bindings
        .iter()
        .enumerate()
        .filter_map(|(idx, (keys, action))| {
            let keys = keys.to_lowercase();
            let action = action.to_lowercase();
            (keys.contains(&q) || action.contains(&q)).then_some(idx)
        })
        .collect()
}

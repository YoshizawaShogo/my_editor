#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Register {
    fragments: Vec<String>,
}

impl Register {
    pub fn store(&mut self, fragments: Vec<String>) {
        self.fragments = fragments;
    }

    pub fn fragments(&self) -> &[String] {
        &self.fragments
    }

    pub fn osc52_text(&self) -> String {
        self.fragments.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

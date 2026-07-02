#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Register {
    fragments: Vec<String>,
    linewise: bool,
}

impl Register {
    pub fn store(&mut self, fragments: Vec<String>) {
        self.fragments = fragments;
        self.linewise = false;
    }

    pub fn store_linewise(&mut self, fragments: Vec<String>) {
        self.fragments = fragments;
        self.linewise = true;
    }

    pub fn fragments(&self) -> &[String] {
        &self.fragments
    }

    pub fn osc52_text(&self) -> String {
        let text = self.fragments.join("\n");
        if self.linewise {
            format!("{text}\n")
        } else {
            text
        }
    }

    pub fn is_linewise(&self) -> bool {
        self.linewise
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linewise_register_adds_one_trailing_newline_for_osc52() {
        let mut register = Register::default();
        register.store_linewise(vec!["one".to_owned(), "two".to_owned()]);

        assert!(register.is_linewise());
        assert_eq!(register.osc52_text(), "one\ntwo\n");
    }
}

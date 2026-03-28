use std::{collections::HashSet, fs, path::PathBuf};

use crate::{error::Result, open_candidate::collect_project_search_paths};

use super::{App, ReplaceField, SearchScope};

impl App {
    pub(super) fn open_or_cycle_replace_input(&mut self) {
        if self.replace_input.active {
            self.cycle_replace_scope();
            return;
        }

        self.replace_input.active = true;
        self.replace_input.find.clear();
        self.replace_input.replace.clear();
        self.replace_input.scope = SearchScope::CurrentFile;
        self.replace_input.field = ReplaceField::Find;
    }

    pub(super) fn close_replace_input(&mut self) {
        self.replace_input.active = false;
        self.replace_input.find.clear();
        self.replace_input.replace.clear();
        self.replace_input.scope = SearchScope::CurrentFile;
        self.replace_input.field = ReplaceField::Find;
    }

    pub(super) fn cycle_replace_scope(&mut self) {
        self.replace_input.scope = match self.replace_input.scope {
            SearchScope::CurrentFile => SearchScope::OpenBuffers,
            SearchScope::OpenBuffers => SearchScope::Project,
            SearchScope::Project => SearchScope::CurrentFile,
        };
    }

    pub(super) fn switch_replace_field(&mut self) {
        self.replace_input.field = match self.replace_input.field {
            ReplaceField::Find => ReplaceField::Replace,
            ReplaceField::Replace => ReplaceField::Find,
        };
    }

    pub(super) fn append_replace_char(&mut self, ch: char) {
        match self.replace_input.field {
            ReplaceField::Find => self.replace_input.find.push(ch),
            ReplaceField::Replace => self.replace_input.replace.push(ch),
        }
    }

    pub(super) fn pop_replace_char(&mut self) {
        match self.replace_input.field {
            ReplaceField::Find => {
                self.replace_input.find.pop();
            }
            ReplaceField::Replace => {
                self.replace_input.replace.pop();
            }
        }
    }

    pub(super) fn submit_replace_input(&mut self) -> Result<()> {
        let find = self.replace_input.find.clone();
        let replace = self.replace_input.replace.clone();
        let scope = self.replace_input.scope;
        self.close_replace_input();

        if find.is_empty() {
            return Ok(());
        }

        let replaced = match scope {
            SearchScope::CurrentFile => self.replace_in_current_document(&find, &replace)?,
            SearchScope::OpenBuffers => self.replace_in_open_buffers(&find, &replace)?,
            SearchScope::Project => self.replace_in_project(&find, &replace)?,
        };

        self.clamp_vertical_state();
        self.show_toast(format!(
            "Replace [{}] {}",
            scope.label(),
            replacement_summary(replaced)
        ));
        Ok(())
    }

    fn replace_in_current_document(&mut self, find: &str, replace: &str) -> Result<usize> {
        if !self.workspace.has_documents() {
            return Ok(0);
        }

        let Some(replaced) = self.workspace.current_document_mut().replace_all(find, replace) else {
            return Ok(0);
        };
        Ok(replaced)
    }

    fn replace_in_open_buffers(&mut self, find: &str, replace: &str) -> Result<usize> {
        let mut total = 0usize;
        for entry in &mut self.workspace.documents {
            let Some(count) = entry.document.replace_all(find, replace) else {
                continue;
            };
            total = total.saturating_add(count);
        }
        Ok(total)
    }

    fn replace_in_project(&mut self, find: &str, replace: &str) -> Result<usize> {
        let mut total = 0usize;
        let project_paths = collect_project_search_paths()?;
        let project_path_set = project_paths.iter().cloned().collect::<HashSet<_>>();
        let mut open_paths = HashSet::<PathBuf>::new();

        for entry in &mut self.workspace.documents {
            if !project_path_set.contains(&entry.path) {
                continue;
            }
            let Some(count) = entry.document.replace_all(find, replace) else {
                continue;
            };
            total = total.saturating_add(count);
            open_paths.insert(entry.path.clone());
            if count > 0 {
                entry.document.save(&entry.path)?;
            }
        }

        for path in project_paths {
            if open_paths.contains(&path) {
                continue;
            }

            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let count = text.match_indices(find).count();
            if count == 0 {
                continue;
            }

            let replaced = text.replace(find, replace);
            fs::write(&path, replaced)?;
            total = total.saturating_add(count);
        }

        let _ = self.refresh_workspace_diagnostic_cache();
        self.poll_lsp();
        Ok(total)
    }
}

fn replacement_summary(count: usize) -> String {
    if count == 1 {
        "1 replacement".to_owned()
    } else {
        format!("{count} replacements")
    }
}

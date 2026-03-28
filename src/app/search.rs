use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{document::Document, error::Result, open_candidate::collect_project_search_paths};

use super::{App, ReplayableAction};

impl App {
    pub(super) fn open_or_cycle_search_input(&mut self) {
        if self.search_input.active {
            self.cycle_search_scope();
            return;
        }

        self.search_input.active = true;
        self.search_input.value.clear();
        self.search_input.scope = super::SearchScope::CurrentFile;
    }

    pub(super) fn cycle_search_scope(&mut self) {
        self.search_input.scope = match self.search_input.scope {
            super::SearchScope::CurrentFile => super::SearchScope::OpenBuffers,
            super::SearchScope::OpenBuffers => super::SearchScope::Project,
            super::SearchScope::Project => super::SearchScope::CurrentFile,
        };
    }

    pub(super) fn incremental_search_current_file(&mut self) {
        if !self.search_input.active
            || self.search_input.scope != super::SearchScope::CurrentFile
            || self.search_input.value.is_empty()
            || !self.workspace.has_documents()
        {
            return;
        }

        if let Ok(Some((document_index, row, column))) =
            self.search_current_file(&self.search_input.value, self.current_page_width())
        {
            self.make_document_current(document_index);
            self.cursor.column = column;
            self.jump_with_context(row, self.current_page_width());
        }
    }

    pub(super) fn close_search_input(&mut self) {
        self.search_input.active = false;
        self.search_input.value.clear();
        self.search_input.scope = super::SearchScope::CurrentFile;
    }

    pub(super) fn submit_search_input(&mut self) -> Result<()> {
        let query = self.search_input.value.clone();
        if query.is_empty() {
            self.close_search_input();
            return Ok(());
        }

        let result = match self.search_input.scope {
            super::SearchScope::CurrentFile => self.search_current_file(&query, self.current_page_width())?,
            super::SearchScope::OpenBuffers => self.search_open_buffers(&query, self.current_page_width())?,
            super::SearchScope::Project => self.search_project_files(&query, self.current_page_width())?,
        };

        if let Some((document_index, row, column)) = result {
            if document_index != self.workspace.current_index {
                self.make_document_current(document_index);
            }
            self.push_jump_history();
            self.cursor.column = column;
            self.last_search = Some(super::SearchState {
                query,
                scope: self.search_input.scope,
            });
            self.jump_with_context(row, self.current_page_width());
        }

        self.close_search_input();
        Ok(())
    }

    pub(super) fn search_current_file(
        &self,
        query: &str,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        Ok(self
            .workspace
            .current_document()
            .first_match_position(query, page_width)
            .map(|(row, column)| (self.workspace.current_index, row, column)))
    }

    pub(super) fn search_open_buffers(
        &self,
        query: &str,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        for (index, entry) in self.workspace.documents.iter().enumerate() {
            if entry.document.is_scratch() {
                continue;
            }
            if let Some((row, column)) = entry.document.first_match_position(query, page_width) {
                return Ok(Some((index, row, column)));
            }
        }

        Ok(None)
    }

    pub(super) fn search_project_files(
        &mut self,
        query: &str,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if self.workspace.has_documents() {
            for (index, entry) in self.workspace.documents.iter().enumerate() {
                if entry.document.is_scratch() {
                    continue;
                }
                if let Some((row, column)) = entry.document.first_match_position(query, page_width) {
                    return Ok(Some((index, row, column)));
                }
            }
        }

        for path in collect_project_search_paths()? {
            if self.workspace.documents.iter().any(|entry| entry.path == path) {
                continue;
            }

            if let Some((line_number, column)) = first_matching_line_number(&path, query)? {
                self.open_document(path.clone())?;
                if let Some(row) = self
                    .workspace
                    .current_document()
                    .jump_row_for_line_number(line_number, page_width)
                {
                    return Ok(Some((self.workspace.current_index, row, column)));
                }
            }
        }

        Ok(None)
    }

    pub(super) fn repeat_search_forward(&mut self) -> Result<()> {
        let Some(search_state) = self.last_search.clone() else {
            return Ok(());
        };
        let page_width = self.current_page_width();

        match search_state.scope {
            super::SearchScope::CurrentFile => {
                if let Some((row, column)) = self.workspace.current_document().next_match_position(
                    &search_state.query,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                ) {
                    self.push_jump_history();
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
            super::SearchScope::OpenBuffers => {
                if let Some((document_index, row, column)) = self.search_open_buffers_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                    true,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
            super::SearchScope::Project => {
                if let Some((document_index, row, column)) = self.search_project_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                    true,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
        }

        Ok(())
    }

    pub(super) fn repeat_search_backward(&mut self) -> Result<()> {
        let Some(search_state) = self.last_search.clone() else {
            return Ok(());
        };
        let page_width = self.current_page_width();

        match search_state.scope {
            super::SearchScope::CurrentFile => {
                if let Some((row, column)) = self.workspace.current_document().previous_match_position(
                    &search_state.query,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                ) {
                    self.push_jump_history();
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
            super::SearchScope::OpenBuffers => {
                if let Some((document_index, row, column)) = self.search_open_buffers_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                    false,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
            super::SearchScope::Project => {
                if let Some((document_index, row, column)) = self.search_project_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                    false,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
        }

        Ok(())
    }

    pub(super) fn search_open_buffers_from(
        &self,
        query: &str,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if forward {
            for (index, entry) in self.workspace.documents.iter().enumerate().skip(start_document_index) {
                if entry.document.is_scratch() {
                    continue;
                }
                let start = if index == start_document_index {
                    entry.document
                        .next_match_position(query, start_row, start_column, page_width)
                } else {
                    entry.document.first_match_position(query, page_width)
                };
                if let Some((row, column)) = start {
                    return Ok(Some((index, row, column)));
                }
            }
        } else {
            for index in (0..=start_document_index).rev() {
                let entry = &self.workspace.documents[index];
                if entry.document.is_scratch() {
                    continue;
                }
                let found = if index == start_document_index {
                    entry
                        .document
                        .previous_match_position(query, start_row, start_column, page_width)
                } else {
                    last_match_in_document(&entry.document, query, page_width)
                };
                if let Some((row, column)) = found {
                    return Ok(Some((index, row, column)));
                }
            }
        }

        Ok(None)
    }

    pub(super) fn search_project_from(
        &mut self,
        query: &str,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if let Some(found) = self.search_open_buffers_from(
            query,
            start_document_index,
            start_row,
            start_column,
            page_width,
            forward,
        )? {
            return Ok(Some(found));
        }

        if !forward {
            return Ok(None);
        }

        for path in collect_project_search_paths()? {
            if self.workspace.documents.iter().any(|entry| entry.path == path) {
                continue;
            }

            if let Some((line_number, column)) = first_matching_line_number(&path, query)? {
                self.open_document(path.clone())?;
                if let Some(row) = self
                    .workspace
                    .current_document()
                    .jump_row_for_line_number(line_number, page_width)
                {
                    return Ok(Some((self.workspace.current_index, row, column)));
                }
            }
        }

        Ok(None)
    }
}

fn first_matching_line_number(path: &Path, query: &str) -> Result<Option<(usize, usize)>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(error.into()),
    };
    let reader = BufReader::new(file);

    for (index, line) in reader.lines().enumerate() {
        let Ok(line) = line else {
            return Ok(None);
        };
        if let Some(column) = line.find(query) {
            return Ok(Some((index + 1, column)));
        }
    }

    Ok(None)
}

fn last_match_in_document(
    document: &Document,
    query: &str,
    page_width: usize,
) -> Option<(usize, usize)> {
    let total_rows = document.total_rows(page_width)?;
    document.previous_match_position(query, total_rows.saturating_sub(1), usize::MAX, page_width)
}

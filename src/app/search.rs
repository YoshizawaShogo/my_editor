use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{
    document::Document,
    error::Result,
    open_candidate::collect_project_search_paths,
    search_options::{Matcher, SearchOptions},
};

use super::{App, ReplayableAction};

impl App {
    /// 検索入力を開く。すでに開いている場合はスコープを循環させる
    pub(super) fn open_or_cycle_search_input(&mut self) {
        if self.search_input.active {
            self.cycle_search_scope();
            return;
        }

        self.search_input = super::SearchInputState {
            active: true,
            ..Default::default()
        };
    }

    /// 検索スコープを CurrentFile → OpenBuffers → Project と循環させる
    pub(super) fn cycle_search_scope(&mut self) {
        self.search_input.scope = match self.search_input.scope {
            super::SearchScope::CurrentFile => super::SearchScope::OpenBuffers,
            super::SearchScope::OpenBuffers => super::SearchScope::Project,
            super::SearchScope::Project => super::SearchScope::CurrentFile,
        };
    }

    /// 入力中のクエリで現在ファイルをインクリメンタル検索し、最初のマッチへカーソルを移動する
    pub(super) fn incremental_search_current_file(&mut self) {
        if !self.search_input.active
            || self.search_input.scope != super::SearchScope::CurrentFile
            || self.search_input.value.is_empty()
            || !self.workspace.has_documents()
        {
            return;
        }

        let query = self.search_input.value.clone();
        let opts = self.search_input.options.clone();
        let page_width = self.current_page_width();
        if let Ok(Some((document_index, row, column))) =
            self.search_current_file(&query, &opts, page_width)
        {
            self.make_document_current(document_index);
            self.cursor.column = column;
            self.jump_with_context(row, page_width);
        }
    }

    /// 検索入力UIを閉じてデフォルト状態にリセットする
    pub(super) fn close_search_input(&mut self) {
        self.search_input = super::SearchInputState::default();
    }

    /// 入力クエリを確定して検索を実行し、最初のマッチ位置へ移動して入力を閉じる
    pub(super) fn submit_search_input(&mut self) -> Result<()> {
        let query = self.search_input.value.clone();
        if query.is_empty() {
            self.close_search_input();
            return Ok(());
        }

        let opts = self.search_input.options.clone();
        let page_width = self.current_page_width();
        let result = match self.search_input.scope {
            super::SearchScope::CurrentFile => {
                self.search_current_file(&query, &opts, page_width)?
            }
            super::SearchScope::OpenBuffers => {
                self.search_open_buffers(&query, &opts, page_width)?
            }
            super::SearchScope::Project => self.search_project_files(&query, &opts, page_width)?,
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
                options: opts,
            });
            self.jump_with_context(row, page_width);
        }

        self.close_search_input();
        Ok(())
    }

    /// 現在のドキュメント先頭からクエリの最初のマッチ位置を返す
    pub(super) fn search_current_file(
        &self,
        query: &str,
        opts: &SearchOptions,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        let Some(matcher) = Matcher::new(query, opts) else {
            return Ok(None);
        };
        Ok(self
            .workspace
            .current_document()
            .first_match_position(&matcher, page_width)
            .map(|(row, column)| (self.workspace.current_index, row, column)))
    }

    /// 開いているすべてのバッファ先頭からクエリの最初のマッチ位置を返す
    pub(super) fn search_open_buffers(
        &self,
        query: &str,
        opts: &SearchOptions,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        let Some(matcher) = Matcher::new(query, opts) else {
            return Ok(None);
        };
        for (index, entry) in self.workspace.documents.iter().enumerate() {
            if entry.document.is_scratch() {
                continue;
            }
            if let Some((row, column)) = entry.document.first_match_position(&matcher, page_width) {
                return Ok(Some((index, row, column)));
            }
        }

        Ok(None)
    }

    /// 開いているバッファを検索し、なければ未読み込みのプロジェクトファイルも検索して最初のマッチ位置を返す
    pub(super) fn search_project_files(
        &mut self,
        query: &str,
        opts: &SearchOptions,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        if let Some(found) = self.search_open_buffers(query, opts, page_width)? {
            return Ok(Some(found));
        }
        self.search_unloaded_project_files(query, opts, page_width)
    }

    /// 未読み込みのプロジェクトファイルを先頭から検索して最初のマッチ位置を返す
    fn search_unloaded_project_files(
        &mut self,
        query: &str,
        opts: &SearchOptions,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        let Some(matcher) = Matcher::new(query, opts) else {
            return Ok(None);
        };
        self.search_unloaded_project_files_with_matcher(&matcher, page_width)
    }

    fn search_unloaded_project_files_with_matcher(
        &mut self,
        matcher: &Matcher,
        page_width: usize,
    ) -> Result<Option<(usize, usize, usize)>> {
        for path in collect_project_search_paths()? {
            if self
                .workspace
                .documents
                .iter()
                .any(|entry| entry.path == path)
            {
                continue;
            }

            if let Some((line_number, column)) = first_matching_line_in_file(&path, matcher)? {
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

    /// 前回の検索を前方向に繰り返す
    pub(super) fn repeat_search_forward(&mut self) -> Result<()> {
        self.repeat_search(true)
    }

    /// 前回の検索を後方向に繰り返す
    pub(super) fn repeat_search_backward(&mut self) -> Result<()> {
        self.repeat_search(false)
    }

    /// 指定方向に前回の検索を繰り返し、マッチ位置へ移動する
    fn repeat_search(&mut self, forward: bool) -> Result<()> {
        let Some(search_state) = self.last_search.clone() else {
            return Ok(());
        };
        let Some(matcher) = Matcher::new(&search_state.query, &search_state.options) else {
            return Ok(());
        };
        let page_width = self.current_page_width();
        let start_column = if forward {
            self.cursor.column.saturating_add(1)
        } else {
            self.cursor.column
        };

        let found = match search_state.scope {
            super::SearchScope::CurrentFile => {
                let position = if forward {
                    self.workspace.current_document().next_match_position(
                        &matcher,
                        self.cursor.row,
                        start_column,
                        page_width,
                    )
                } else {
                    self.workspace.current_document().previous_match_position(
                        &matcher,
                        self.cursor.row,
                        start_column,
                        page_width,
                    )
                };
                position.map(|(row, column)| (self.workspace.current_index, row, column))
            }
            super::SearchScope::OpenBuffers => self.search_open_buffers_from(
                &matcher,
                self.workspace.current_index,
                self.cursor.row,
                start_column,
                page_width,
                forward,
            )?,
            super::SearchScope::Project => self.search_project_from(
                &matcher,
                self.workspace.current_index,
                self.cursor.row,
                start_column,
                page_width,
                forward,
            )?,
        };

        if let Some((document_index, row, column)) = found {
            self.push_jump_history();
            self.make_document_current(document_index);
            self.cursor.column = column;
            self.jump_with_context(row, page_width);
            self.last_replayable_action = Some(ReplayableAction::Search { forward });
        }

        Ok(())
    }

    /// 指定位置から開いているバッファを指定方向に検索し、マッチ位置を返す
    pub(super) fn search_open_buffers_from(
        &self,
        matcher: &Matcher,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if forward {
            for (index, entry) in self
                .workspace
                .documents
                .iter()
                .enumerate()
                .skip(start_document_index)
            {
                if entry.document.is_scratch() {
                    continue;
                }
                let start = if index == start_document_index {
                    entry
                        .document
                        .next_match_position(matcher, start_row, start_column, page_width)
                } else {
                    entry.document.first_match_position(matcher, page_width)
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
                    entry.document.previous_match_position(
                        matcher,
                        start_row,
                        start_column,
                        page_width,
                    )
                } else {
                    last_match_in_document(&entry.document, matcher, page_width)
                };
                if let Some((row, column)) = found {
                    return Ok(Some((index, row, column)));
                }
            }
        }

        Ok(None)
    }

    /// 指定位置からバッファおよびプロジェクトファイルを指定方向に検索し、マッチ位置を返す
    pub(super) fn search_project_from(
        &mut self,
        matcher: &Matcher,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if let Some(found) = self.search_open_buffers_from(
            matcher,
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

        self.search_unloaded_project_files_with_matcher(matcher, page_width)
    }
}

/// ファイルを行ごとに読み、マッチャーに最初にマッチする行番号(1始まり)と列を返す
fn first_matching_line_in_file(path: &Path, matcher: &Matcher) -> Result<Option<(usize, usize)>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    for (index, line) in reader.lines().enumerate() {
        let Ok(line) = line else {
            return Ok(None);
        };
        if let Some(column) = matcher.find(&line) {
            return Ok(Some((index + 1, column)));
        }
    }

    Ok(None)
}

/// ドキュメント末尾からマッチャーの最後のマッチ位置を返す
fn last_match_in_document(
    document: &Document,
    matcher: &Matcher,
    page_width: usize,
) -> Option<(usize, usize)> {
    let total_rows = document.total_rows(page_width)?;
    document.previous_match_position(
        matcher,
        total_rows.saturating_sub(1),
        usize::MAX,
        page_width,
    )
}

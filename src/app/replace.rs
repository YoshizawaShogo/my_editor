use std::{collections::HashSet, fs, path::PathBuf};

use crate::{error::Result, open_candidate::collect_project_search_paths, search_options::Matcher};

use super::{App, ReplaceField, ReplaceInputState, SearchScope};

impl App {
    /// 置換入力を開く。すでに開いている場合はスコープを循環させる
    pub(super) fn open_or_cycle_replace_input(&mut self) {
        if self.replace_input.active {
            self.cycle_replace_scope();
            return;
        }

        self.replace_input = ReplaceInputState {
            active: true,
            ..Default::default()
        };
    }

    /// 置換入力UIを閉じてデフォルト状態にリセットする
    pub(super) fn close_replace_input(&mut self) {
        self.replace_input = ReplaceInputState::default();
    }

    /// 置換スコープを CurrentFile → OpenBuffers → Project と循環させる
    pub(super) fn cycle_replace_scope(&mut self) {
        self.replace_input.scope = match self.replace_input.scope {
            SearchScope::CurrentFile => SearchScope::OpenBuffers,
            SearchScope::OpenBuffers => SearchScope::Project,
            SearchScope::Project => SearchScope::CurrentFile,
        };
    }

    /// フォーカスを Find フィールドと Replace フィールドで切り替える
    pub(super) fn switch_replace_field(&mut self) {
        self.replace_input.field = match self.replace_input.field {
            ReplaceField::Find => ReplaceField::Replace,
            ReplaceField::Replace => ReplaceField::Find,
        };
    }

    /// フォーカス中のフィールドの末尾に文字を追加する
    pub(super) fn append_replace_char(&mut self, ch: char) {
        match self.replace_input.field {
            ReplaceField::Find => self.replace_input.find.push(ch),
            ReplaceField::Replace => self.replace_input.replace.push(ch),
        }
    }

    /// フォーカス中のフィールドの末尾の文字を削除する
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

    /// 入力内容を確定してスコープに応じた置換を実行し、結果をトーストで通知する
    pub(super) fn submit_replace_input(&mut self) -> Result<()> {
        let find = self.replace_input.find.clone();
        let replace = self.replace_input.replace.clone();
        let scope = self.replace_input.scope;
        let opts = self.replace_input.options.clone();
        self.close_replace_input();

        if find.is_empty() {
            return Ok(());
        }

        let Some(matcher) = Matcher::new(&find, &opts) else {
            return Ok(());
        };

        let replaced = match scope {
            SearchScope::CurrentFile => self.replace_in_current_document(&matcher, &replace)?,
            SearchScope::OpenBuffers => self.replace_in_open_buffers(&matcher, &replace)?,
            SearchScope::Project => self.replace_in_project(&matcher, &replace)?,
        };

        self.clamp_vertical_state();
        self.show_toast(format!(
            "Replace [{}] {}",
            scope.label(),
            replacement_summary(replaced)
        ));
        Ok(())
    }

    /// 現在のドキュメント内でマッチャーで全置換し、置換件数を返す
    fn replace_in_current_document(&mut self, matcher: &Matcher, replace: &str) -> Result<usize> {
        if !self.workspace.has_documents() {
            return Ok(0);
        }

        let Some(replaced) = self
            .workspace
            .current_document_mut()
            .replace_all(matcher, replace)
        else {
            return Ok(0);
        };
        Ok(replaced)
    }

    /// 開いているすべてのバッファ内でマッチャーで全置換し、合計置換件数を返す
    fn replace_in_open_buffers(&mut self, matcher: &Matcher, replace: &str) -> Result<usize> {
        let mut total = 0usize;
        for entry in &mut self.workspace.documents {
            let Some(count) = entry.document.replace_all(matcher, replace) else {
                continue;
            };
            total = total.saturating_add(count);
        }
        Ok(total)
    }

    /// プロジェクト全体でマッチャーで全置換し、置換後にLSPキャッシュを更新する
    fn replace_in_project(&mut self, matcher: &Matcher, replace: &str) -> Result<usize> {
        let project_paths = collect_project_search_paths()?;
        let project_path_set: HashSet<_> = project_paths.iter().cloned().collect();

        let (open_total, open_paths) =
            self.replace_in_open_project_buffers(matcher, replace, &project_path_set)?;
        let unloaded_total =
            replace_in_unloaded_project_files(matcher, replace, &project_paths, &open_paths)?;

        let _ = self.refresh_workspace_diagnostic_cache();
        self.poll_lsp();
        Ok(open_total.saturating_add(unloaded_total))
    }

    /// プロジェクトに属する開いているバッファをマッチャーで全置換し、変更があればディスクに保存する
    fn replace_in_open_project_buffers(
        &mut self,
        matcher: &Matcher,
        replace: &str,
        project_path_set: &HashSet<PathBuf>,
    ) -> Result<(usize, HashSet<PathBuf>)> {
        let mut total = 0usize;
        let mut replaced_paths = HashSet::new();

        for entry in &mut self.workspace.documents {
            if !project_path_set.contains(&entry.path) {
                continue;
            }
            let Some(count) = entry.document.replace_all(matcher, replace) else {
                continue;
            };
            total = total.saturating_add(count);
            replaced_paths.insert(entry.path.clone());
            if count > 0 {
                entry.document.save(&entry.path)?;
            }
        }

        Ok((total, replaced_paths))
    }
}

/// 未読み込みのプロジェクトファイルをファイルI/Oでマッチャーを使って全置換し、合計置換件数を返す
fn replace_in_unloaded_project_files(
    matcher: &Matcher,
    replace: &str,
    project_paths: &[PathBuf],
    skip_paths: &HashSet<PathBuf>,
) -> Result<usize> {
    let mut total = 0usize;

    for path in project_paths {
        if skip_paths.contains(path) {
            continue;
        }

        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };

        let mut result = String::new();
        let mut last_end = 0usize;
        let mut count = 0usize;
        let mut search_from = 0usize;
        while let Some((start, end)) = matcher.find_match_from(&text, search_from) {
            result.push_str(&text[last_end..start]);
            result.push_str(replace);
            last_end = end;
            search_from = if end > start { end } else { end + 1 };
            count += 1;
        }
        if count == 0 {
            continue;
        }
        result.push_str(&text[last_end..]);
        fs::write(path, result)?;
        total = total.saturating_add(count);
    }

    Ok(total)
}

/// 置換件数を人間が読める文字列に変換する
fn replacement_summary(count: usize) -> String {
    if count == 1 {
        "1 replacement".to_owned()
    } else {
        format!("{count} replacements")
    }
}

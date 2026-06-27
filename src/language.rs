use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

/// ~/.my_editor_rc.toml の内容
///
/// ```toml
/// [get_lang]
/// rs = "rust"
/// ts = "typescript"
///
/// [lang_lsp_map]
/// rust = "rust-analyzer"
/// typescript = "typescript-language-server --stdio"
/// ```
#[derive(Deserialize, Default)]
struct RcFile {
    #[serde(default)]
    get_lang: HashMap<String, String>,
    #[serde(default)]
    lang_lsp_map: HashMap<String, String>,
}

pub struct EditorRc {
    get_lang: HashMap<String, String>,
    lang_lsp_map: HashMap<String, String>,
}

impl EditorRc {
    pub fn load() -> Self {
        let rc = rc_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str::<RcFile>(&s).ok())
            .unwrap_or_default();
        Self {
            get_lang: rc.get_lang,
            lang_lsp_map: rc.lang_lsp_map,
        }
    }

    /// パスの拡張子から LSP コマンドと引数を返す
    pub fn lsp_for_path(&self, path: &Path) -> Option<(String, Vec<String>)> {
        let ext = path.extension()?.to_str()?;
        let lang = self.get_lang.get(ext)?;
        let command_line = self.lang_lsp_map.get(lang)?;
        let mut parts = command_line.split_whitespace();
        let cmd = parts.next()?.to_owned();
        let args = parts.map(str::to_owned).collect();
        Some((cmd, args))
    }
}

fn rc_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".my_editor_rc.toml"))
}

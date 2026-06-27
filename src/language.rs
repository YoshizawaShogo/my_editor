use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct LanguageEntry {
    pub extension: String,
    pub lsp_command: Option<String>,
    #[serde(default)]
    pub lsp_args: Vec<String>,
    pub lsp_init_options: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct LanguageConfigFile {
    #[serde(default)]
    language: Vec<LanguageEntry>,
}

/// 設定ファイルのパス: ~/.config/my_editor/languages.toml
fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/my_editor/languages.toml"))
}

/// 設定ファイルを読む。存在しなければ組み込みデフォルト（rs → rust-analyzer）を返す。
pub fn load_language_config() -> Vec<LanguageEntry> {
    if let Some(path) = config_path()
        && let Ok(contents) = std::fs::read_to_string(&path)
        && let Ok(file) = toml::from_str::<LanguageConfigFile>(&contents)
    {
        return file.language;
    }
    default_language_config()
}

fn default_language_config() -> Vec<LanguageEntry> {
    vec![LanguageEntry {
        extension: "rs".to_owned(),
        lsp_command: Some("rust-analyzer".to_owned()),
        lsp_args: vec![],
        lsp_init_options: None,
    }]
}

/// 拡張子でマッチする LanguageEntry を返す
pub fn detect_language<'a>(path: &Path, config: &'a [LanguageEntry]) -> Option<&'a LanguageEntry> {
    let ext = path.extension()?.to_str()?;
    config.iter().find(|e| e.extension == ext)
}

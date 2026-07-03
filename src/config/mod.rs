use std::path::Path;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct Config {
    pub language: Vec<LanguageConfig>,
    pub editor: EditorConfig,
    pub search: SearchConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut rust = LanguageConfig::new("rust", &["rs"], Some("//"));
        rust.lsp = Some(vec!["rust-analyzer".to_owned()]);
        Self {
            language: vec![
                rust,
                LanguageConfig::new("toml", &["toml"], Some("#")),
                LanguageConfig::new("markdown", &["md", "markdown"], None),
                LanguageConfig::new("json", &["json"], Some("//")),
                LanguageConfig {
                    name: "make".to_owned(),
                    filenames: vec![
                        "Makefile".to_owned(),
                        "makefile".to_owned(),
                        "GNUmakefile".to_owned(),
                    ],
                    insert_spaces: Some(false),
                    tab_size: Some(4),
                    ..LanguageConfig::default()
                },
            ],
            editor: EditorConfig::default(),
            search: SearchConfig::default(),
        }
    }
}

impl Config {
    pub fn merged_with_defaults(self) -> Self {
        let Self {
            language,
            editor,
            search,
        } = self;
        let mut merged = Self {
            editor,
            search,
            ..Self::default()
        };
        for language in language {
            if let Some(existing) = merged
                .language
                .iter_mut()
                .find(|existing| existing.name == language.name)
            {
                *existing = language;
            } else {
                merged.language.push(language);
            }
        }
        merged
    }

    pub fn language_for_path(&self, path: &Path) -> Option<&LanguageConfig> {
        let filename = path.file_name().and_then(|name| name.to_str());
        let extension = path.extension().and_then(|extension| extension.to_str());
        self.language.iter().find(|language| {
            filename.is_some_and(|filename| {
                language
                    .filenames
                    .iter()
                    .any(|candidate| candidate == filename)
            }) || extension.is_some_and(|extension| {
                language
                    .extensions
                    .iter()
                    .any(|candidate| candidate == extension)
            })
        })
    }

    pub fn indentation_for_language(&self, language: Option<&str>) -> (usize, bool) {
        let language =
            language.and_then(|name| self.language.iter().find(|language| language.name == name));
        (
            language
                .and_then(|language| language.tab_size)
                .unwrap_or(self.editor.tab_size)
                .max(1),
            language
                .and_then(|language| language.insert_spaces)
                .unwrap_or(self.editor.insert_spaces),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct LanguageConfig {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub lsp: Option<Vec<String>>,
    pub line_comment: Option<String>,
    pub tab_size: Option<usize>,
    pub insert_spaces: Option<bool>,
}

impl LanguageConfig {
    fn new(name: &str, extensions: &[&str], line_comment: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            extensions: extensions.iter().map(|value| (*value).to_owned()).collect(),
            filenames: Vec::new(),
            lsp: None,
            line_comment: line_comment.map(str::to_owned),
            tab_size: None,
            insert_spaces: None,
        }
    }
}

impl Default for LanguageConfig {
    fn default() -> Self {
        Self::new("text", &[], None)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct EditorConfig {
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub shell: Option<String>,
    pub large_file_threshold: String,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
            shell: None,
            large_file_threshold: "10MiB".to_owned(),
        }
    }
}

impl EditorConfig {
    pub fn large_file_threshold_bytes(&self) -> u64 {
        parse_size(&self.large_file_threshold).unwrap_or(10 * 1024 * 1024)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct SearchConfig {
    pub respect_ignore_files: bool,
    pub include_hidden: bool,
    pub exclude: Vec<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            respect_ignore_files: true,
            include_hidden: false,
            exclude: vec![
                "**/.git/**".to_owned(),
                "**/target/**".to_owned(),
                "**/node_modules/**".to_owned(),
            ],
        }
    }
}

fn parse_size(value: &str) -> Option<u64> {
    let value = value.trim();
    for (suffix, multiplier) in [("MiB", 1024 * 1024), ("KiB", 1024), ("B", 1)] {
        if let Some(number) = value.strip_suffix(suffix) {
            return number.trim().parse::<u64>().ok()?.checked_mul(multiplier);
        }
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_and_size() {
        let config: Config = toml::from_str(
            r#"
                [editor]
                tab_size = 2
                insert_spaces = true
                shell = "/bin/zsh"
                large_file_threshold = "3MiB"
            "#,
        )
        .unwrap();

        assert_eq!(config.editor.tab_size, 2);
        assert!(config.editor.insert_spaces);
        assert_eq!(config.editor.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(config.editor.large_file_threshold_bytes(), 3 * 1024 * 1024);
    }

    #[test]
    fn user_languages_extend_defaults_instead_of_removing_rust() {
        let config: Config = toml::from_str(
            r##"
                [[language]]
                name = "python"
                extensions = ["py"]
                lsp = ["pylsp"]
                line_comment = "#"
            "##,
        )
        .unwrap();
        let config = config.merged_with_defaults();

        assert_eq!(
            config.language_for_path(Path::new("main.rs")).unwrap().name,
            "rust"
        );
        assert_eq!(
            config.language_for_path(Path::new("main.py")).unwrap().name,
            "python"
        );
    }

    #[test]
    fn makefile_uses_real_tabs_and_language_overrides_can_change_width() {
        let config = Config::default();
        let make = config.language_for_path(Path::new("Makefile")).unwrap();

        assert_eq!(make.name, "make");
        assert_eq!(
            config.indentation_for_language(Some(&make.name)),
            (4, false)
        );

        let config: Config = toml::from_str(
            r#"
                [editor]
                tab_size = 2
                insert_spaces = true

                [[language]]
                name = "make"
                filenames = ["Makefile"]
                tab_size = 8
                insert_spaces = false
            "#,
        )
        .unwrap();
        assert_eq!(config.indentation_for_language(Some("make")), (8, false));
    }
}

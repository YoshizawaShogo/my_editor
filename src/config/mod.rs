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
                LanguageConfig {
                    // Cargo.lock is TOML under a name of its own.
                    filenames: vec!["Cargo.lock".to_owned()],
                    ..LanguageConfig::new("toml", &["toml"], Some("#"))
                },
                LanguageConfig::new("markdown", &["md", "markdown"], None),
                LanguageConfig::new("json", &["json", "jsonc"], Some("//")),
                LanguageConfig {
                    lsp: Some(vec!["pylsp".to_owned()]),
                    // .pyi: type stubs; .pyw: windowed scripts on Windows.
                    ..LanguageConfig::new("python", &["py", "pyi", "pyw"], Some("#"))
                },
                LanguageConfig {
                    lsp: Some(vec!["clangd".to_owned()]),
                    ..LanguageConfig::new("c", &["c", "h"], Some("//"))
                },
                LanguageConfig {
                    // Startup files have no extension; they are matched by name.
                    filenames: [
                        ".bashrc",
                        ".bash_profile",
                        ".bash_aliases",
                        ".bash_logout",
                        ".profile",
                    ]
                    .map(str::to_owned)
                    .into(),
                    ..LanguageConfig::new("bash", &["sh", "bash"], Some("#"))
                },
                // csh is its own language (its control flow differs from bash, so
                // snippets and shellcheck must treat it separately) and is syntax
                // highlighted by a regex pass rather than tree-sitter (see
                // highlight::csh).
                LanguageConfig {
                    filenames: [".cshrc", ".tcshrc", ".login", ".logout"]
                        .map(str::to_owned)
                        .into(),
                    ..LanguageConfig::new("csh", &["csh", "tcsh"], Some("#"))
                },
                // Highlighted via the bca-tree-sitter-tcl grammar with a vendored
                // query (see highlight::grammar / tcl_highlights.scm).
                // EDA tool inputs written in Tcl ride along: timing constraints
                // (.sdc, Xilinx .xdc), power intent (.upf, .cpf) and simulator
                // do-files (.do) are Tcl command scripts, so the grammar fits.
                LanguageConfig::new(
                    "tcl",
                    &["tcl", "tk", "itcl", "tm", "sdc", "xdc", "upf", "cpf", "do"],
                    Some("#"),
                ),
                LanguageConfig {
                    name: "make".to_owned(),
                    extensions: vec!["mk".to_owned(), "mak".to_owned()],
                    filenames: vec![
                        "Makefile".to_owned(),
                        "makefile".to_owned(),
                        "GNUmakefile".to_owned(),
                    ],
                    insert_spaces: Some(false),
                    tab_size: Some(4),
                    // So Ctrl+/ comments a line out instead of reporting that
                    // make has no comment syntax.
                    line_comment: Some("#".to_owned()),
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

    /// The language a script's `#!` line names. A script runs under that
    /// interpreter whatever it is called, so this wins over the file's name —
    /// a tcsh script called `setup` or even `setup.sh` is still tcsh. Only
    /// interpreters with a language here count; anything else leaves the
    /// decision to the name.
    pub fn language_for_shebang(&self, first_line: &str) -> Option<&LanguageConfig> {
        let mut words = first_line.strip_prefix("#!")?.split_whitespace();
        let mut program = words.next()?;
        // `#!/usr/bin/env tcsh` (or `env -S tcsh -f`): the program is the one
        // env goes on to run.
        if program.rsplit('/').next() == Some("env") {
            program = words.find(|word| !word.starts_with('-'))?;
        }
        let name = program.rsplit('/').next()?;
        // Versioned names count as the plain one: `python3.12`, `tclsh8.6`.
        let language = match name
            .trim_end_matches(|character: char| character.is_ascii_digit() || character == '.')
        {
            "tcsh" | "csh" => "csh",
            "bash" | "sh" | "dash" | "ksh" => "bash",
            "python" => "python",
            "tclsh" | "wish" => "tcl",
            "make" | "gmake" => "make",
            _ => return None,
        };
        self.language.iter().find(|config| config.name == language)
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
            // Directory names pruned from every search, applied behind the scenes
            // rather than shown in the exclude field.
            exclude: vec![
                ".git".to_owned(),
                "target".to_owned(),
                "node_modules".to_owned(),
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
    fn a_shebang_names_the_language_whatever_the_file_is_called() {
        let config = Config::default();
        let language = |line: &str| {
            config
                .language_for_shebang(line)
                .map(|language| language.name.as_str())
        };

        assert_eq!(language("#!/bin/tcsh"), Some("csh"));
        assert_eq!(language("#!/bin/csh -f"), Some("csh"));
        assert_eq!(language("#!/usr/bin/env tcsh"), Some("csh"));
        assert_eq!(language("#!/usr/bin/env -S tcsh -f"), Some("csh"));
        assert_eq!(language("#! /bin/sh"), Some("bash"));
        assert_eq!(language("#!/usr/bin/env python3"), Some("python"));
        assert_eq!(language("#!/usr/bin/tclsh8.6"), Some("tcl"));
        assert_eq!(language("#!/usr/bin/make -f"), Some("make"));
        // An interpreter with no language here, or no shebang at all, leaves the
        // decision to the file's name.
        assert_eq!(language("#!/usr/bin/perl"), None);
        assert_eq!(language("set x = 1"), None);
    }

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
    fn startup_files_and_tool_dialects_resolve_to_their_language() {
        let config = Config::default();
        let language = |path: &str| {
            config
                .language_for_path(Path::new(path))
                .map(|language| language.name.clone())
        };
        // Dot-files have no extension as far as Path::extension is concerned, so
        // they only resolve through the file-name list.
        assert_eq!(language("/home/u/.cshrc").as_deref(), Some("csh"));
        assert_eq!(language("/home/u/.bashrc").as_deref(), Some("bash"));
        // Stubs, and EDA inputs that are Tcl scripts under their own extension.
        assert_eq!(language("typing.pyi").as_deref(), Some("python"));
        assert_eq!(language("top.sdc").as_deref(), Some("tcl"));
        assert_eq!(language("run.do").as_deref(), Some("tcl"));
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

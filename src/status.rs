//! `my_editor --status`: report, per language, whether the external tools it
//! relies on are installed. The language servers and helpers like shellcheck and
//! ctags are a user-installed prerequisite (the editor never bundles them), so
//! this is how you confirm a machine is set up without launching the TUI.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::Config;

/// A tool a language can use, beyond the language server named in its config.
struct Tool {
    command: &'static str,
    role: &'static str,
}

/// Language-specific helpers that are not expressed as an LSP command. Kept as a
/// compile-time table so the set of tools a language wants lives in one place.
const LANGUAGE_TOOLS: &[(&str, &[Tool])] = &[(
    "bash",
    &[Tool {
        command: "shellcheck",
        role: "linter",
    }],
)];

/// Tools that help across every language (definition tags, …).
const GENERAL_TOOLS: &[Tool] = &[Tool {
    command: "ctags",
    role: "tags",
}];

/// Resolve `command` to an absolute path by scanning `PATH`. Never executes it.
pub fn which(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|directory| {
        let candidate = directory.join(command);
        is_executable(&candidate).then_some(candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A human-readable report of tool availability, grouped by language. `resolve`
/// maps a command name to its path (injected so formatting stays testable off a
/// real filesystem).
pub fn tool_report(config: &Config, resolve: impl Fn(&str) -> Option<PathBuf>) -> String {
    let mut out = String::new();
    for language in &config.language {
        let lsp = language.lsp.as_ref().and_then(|command| command.first());
        let extras = language_tools(&language.name);
        // Languages with nothing external to check (plain highlight-only) are
        // omitted so the report shows only what a user might need to install.
        if lsp.is_none() && extras.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}", language.name);
        if let Some(command) = lsp {
            write_tool(&mut out, command, "LSP", &resolve);
        }
        for tool in extras {
            write_tool(&mut out, tool.command, tool.role, &resolve);
        }
    }
    let _ = writeln!(out, "general");
    for tool in GENERAL_TOOLS {
        write_tool(&mut out, tool.command, tool.role, &resolve);
    }
    out
}

fn language_tools(name: &str) -> &'static [Tool] {
    for (language, tools) in LANGUAGE_TOOLS {
        if *language == name {
            return tools;
        }
    }
    &[]
}

fn write_tool(
    out: &mut String,
    command: &str,
    role: &str,
    resolve: &impl Fn(&str) -> Option<PathBuf>,
) {
    match resolve(command) {
        Some(path) => {
            let _ = writeln!(out, "  ✓ {command} ({role})  {}", path.display());
        }
        None => {
            let _ = writeln!(out, "  ✗ {command} ({role})  not installed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn report_groups_tools_by_language_and_marks_what_is_missing() {
        let config = Config::default();
        // Pretend rust-analyzer and shellcheck are installed, nothing else is.
        let installed: HashSet<&str> = ["rust-analyzer", "shellcheck"].into_iter().collect();
        let report = tool_report(&config, |command| {
            installed
                .contains(command)
                .then(|| PathBuf::from(format!("/usr/bin/{command}")))
        });

        assert!(report.contains("rust\n  ✓ rust-analyzer (LSP)"));
        // bash has no LSP configured but still reports its shellcheck helper.
        assert!(report.contains("bash\n  ✓ shellcheck (linter)"));
        // An LSP the machine lacks is shown as missing, not hidden.
        assert!(report.contains("✗ clangd (LSP)"));
        // ctags is a cross-language helper listed once under `general`.
        assert!(report.contains("general\n  ✗ ctags (tags)"));
        // A highlight-only language with no external tools is not listed.
        assert!(!report.contains("markdown"));
    }
}

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

/// Truecolor (24-bit) ANSI styling for the report, in the editor's Iceberg
/// palette. Every field is empty when colour is off, so the same formatting code
/// produces plain text for a pipe/file or when `NO_COLOR` is set.
struct Palette {
    header: &'static str,
    ok: &'static str,
    missing: &'static str,
    dim: &'static str,
    reset: &'static str,
}

impl Palette {
    fn new(color: bool) -> Self {
        if color {
            Self {
                header: "\x1b[1;38;2;132;160;198m", // bold Iceberg blue
                ok: "\x1b[38;2;180;190;130m",       // green
                missing: "\x1b[38;2;226;120;120m",  // red
                dim: "\x1b[38;2;107;112;137m",      // muted
                reset: "\x1b[0m",
            }
        } else {
            Self {
                header: "",
                ok: "",
                missing: "",
                dim: "",
                reset: "",
            }
        }
    }
}

/// A human-readable report of tool availability, grouped by language, drawn in a
/// box with aligned columns. `resolve` maps a command name to its path (injected
/// so formatting stays testable off a real filesystem). `color` turns on
/// truecolor styling — callers pass it only for a terminal, so piped/redirected
/// output stays plain (the box is drawn with standard box-drawing characters, no
/// Nerd Font required).
pub fn tool_report(
    config: &Config,
    resolve: impl Fn(&str) -> Option<PathBuf>,
    color: bool,
) -> String {
    let palette = Palette::new(color);

    // Gather the sections up front so column widths can be sized to the content.
    let mut sections: Vec<(String, Vec<(String, &'static str)>)> = Vec::new();
    for language in &config.language {
        let lsp = language.lsp.as_ref().and_then(|command| command.first());
        let extras = language_tools(&language.name);
        // Languages with nothing external to check (plain highlight-only) are
        // omitted so the report shows only what a user might need to install.
        if lsp.is_none() && extras.is_empty() {
            continue;
        }
        let mut rows: Vec<(String, &'static str)> = Vec::new();
        if let Some(command) = lsp {
            rows.push((command.clone(), "LSP"));
        }
        rows.extend(
            extras
                .iter()
                .map(|tool| (tool.command.to_owned(), tool.role)),
        );
        sections.push((language.name.clone(), rows));
    }
    sections.push((
        "general".to_owned(),
        GENERAL_TOOLS
            .iter()
            .map(|tool| (tool.command.to_owned(), tool.role))
            .collect(),
    ));

    let name_w = column_width(&sections, |(command, _)| command.chars().count());
    let role_w = column_width(&sections, |(_, role)| role.chars().count());

    // Each entry is (visible width, styled text) so the box can be padded off the
    // visible width while the styled text carries the (zero-width) colour codes.
    let mut lines: Vec<(usize, String)> = vec![(0, String::new())];
    for (name, rows) in &sections {
        lines.push((
            name.chars().count(),
            format!("{}{name}{}", palette.header, palette.reset),
        ));
        for (command, role) in rows {
            let (mark, mark_color, tail, tail_color) = match resolve(command) {
                Some(path) => ("✓", palette.ok, path.display().to_string(), palette.dim),
                None => (
                    "✗",
                    palette.missing,
                    "not installed".to_owned(),
                    palette.missing,
                ),
            };
            let plain = format!("  {mark} {command:name_w$}  {role:role_w$}  {tail}");
            lines.push((
                plain.chars().count(),
                format!(
                    "  {mark_color}{mark}{reset} {command:name_w$}  {dim}{role:role_w$}{reset}  {tail_color}{tail}{reset}",
                    reset = palette.reset,
                    dim = palette.dim,
                ),
            ));
        }
    }
    lines.push((0, String::new()));

    render_box("my_editor · tool status", &lines, &palette)
}

/// Widest value produced by `field` across every row in every section.
fn column_width(
    sections: &[(String, Vec<(String, &'static str)>)],
    field: impl Fn(&(String, &'static str)) -> usize,
) -> usize {
    sections
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(field)
        .max()
        .unwrap_or(0)
}

/// Frame `lines` (each already `(visible width, styled text)`) in a titled box.
fn render_box(title: &str, lines: &[(usize, String)], palette: &Palette) -> String {
    let content_w = lines.iter().map(|(width, _)| *width).max().unwrap_or(0);
    // The run between the corners: content plus one space of padding each side,
    // widened if the title needs more room ("─ " + title + " ").
    let title_run = title.chars().count() + 3;
    let bar = (content_w + 2).max(title_run);
    let (border, reset, header) = (palette.dim, palette.reset, palette.header);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{border}┌─ {reset}{header}{title}{reset} {border}{}┐{reset}",
        "─".repeat(bar - title_run)
    );
    for (width, styled) in lines {
        let _ = writeln!(
            out,
            "{border}│{reset} {styled}{} {border}│{reset}",
            " ".repeat(bar - 2 - width)
        );
    }
    let _ = writeln!(out, "{border}└{}┘{reset}", "─".repeat(bar));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn report_groups_tools_by_language_and_marks_what_is_missing() {
        let config = Config::default();
        // Pretend rust-analyzer and shellcheck are installed, nothing else is.
        let installed: HashSet<&str> = ["rust-analyzer", "shellcheck"].into_iter().collect();
        let report = tool_report(
            &config,
            |command| {
                installed
                    .contains(command)
                    .then(|| PathBuf::from(format!("/usr/bin/{command}")))
            },
            false,
        );

        assert!(report.contains("rust"));
        assert!(report.contains("✓ rust-analyzer"));
        // bash has no LSP configured but still reports its shellcheck helper.
        assert!(report.contains("✓ shellcheck"));
        // An LSP the machine lacks is shown as missing, not hidden.
        assert!(report.contains("✗ clangd"));
        // ctags is a cross-language helper listed once under `general`.
        assert!(report.contains("general"));
        assert!(report.contains("✗ ctags"));
        // A highlight-only language with no external tools is not listed.
        assert!(!report.contains("markdown"));
        // Plain mode emits no ANSI escapes.
        assert!(!report.contains('\x1b'));
    }

    #[test]
    fn color_mode_emits_truecolor_escapes() {
        let report = tool_report(&Config::default(), |_| None, true);
        // 24-bit foreground sequences (\x1b[38;2;R;G;Bm) are present when styled.
        assert!(report.contains("\x1b[38;2;"));
    }
}

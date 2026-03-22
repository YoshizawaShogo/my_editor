use ratatui::style::Color;

pub struct AppColors;

impl AppColors {
    pub const BACKGROUND: Color = Color::Rgb(8, 8, 10);
    pub const FOREGROUND: Color = Color::Rgb(196, 198, 204);
    pub const MUTED: Color = Color::Rgb(146, 151, 162);
    pub const INDENT_GUIDE: Color = Color::Rgb(78, 84, 94);
    pub const ACCENT: Color = Color::Rgb(166, 189, 214);
    pub const SEARCH_HIGHLIGHT: Color = Color::Rgb(180, 155, 92);
    pub const GIT_ADDED: Color = Color::Rgb(137, 178, 141);
    pub const GIT_MODIFIED: Color = Color::Rgb(122, 162, 247);
    pub const GIT_REMOVED: Color = Color::Rgb(191, 121, 121);
    pub const DIAGNOSTIC_WARNING: Color = Color::Rgb(219, 184, 116);
    pub const DIAGNOSTIC_ERROR: Color = Color::Rgb(224, 108, 117);
    pub const PANEL: Color = Color::Rgb(20, 22, 26);
    pub const PANEL_ALT: Color = Color::Rgb(28, 31, 36);
    pub const PANEL_SOFT: Color = Color::Rgb(36, 40, 46);
    pub const NORMAL_MODE: Color = Color::Rgb(128, 143, 167);
    pub const INSERT_MODE: Color = Color::Rgb(162, 187, 152);
    pub const COMMAND_MODE: Color = Color::Rgb(204, 176, 138);
    pub const SHELL_MODE: Color = Color::Rgb(176, 154, 196);
}

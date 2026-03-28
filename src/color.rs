use ratatui::style::Color;

pub struct AppColors;

impl AppColors {
    pub const BACKGROUND: Color = Color::Rgb(8, 8, 10);
    pub const FOREGROUND: Color = Color::Rgb(196, 198, 204);
    pub const MUTED: Color = Color::Rgb(146, 151, 162);
    pub const CURRENT_LINE_NUMBER: Color = Color::Rgb(182, 186, 194);
    pub const INDENT_GUIDE: Color = Color::Rgb(78, 84, 94);
    pub const ACCENT: Color = Color::Rgb(166, 189, 214);
    pub const SEARCH_HIGHLIGHT: Color = Color::Rgb(180, 155, 92);
    pub const SELECTION_HIGHLIGHT: Color = Color::Rgb(64, 84, 128);
    pub const GIT_ADDED: Color = Color::Rgb(137, 178, 141);
    pub const GIT_MODIFIED: Color = Color::Rgb(122, 162, 247);
    pub const GIT_REMOVED: Color = Color::Rgb(191, 121, 121);
    pub const DIAGNOSTIC_WARNING: Color = Color::Rgb(219, 184, 116);
    pub const DIAGNOSTIC_ERROR: Color = Color::Rgb(224, 108, 117);
    pub const PANEL: Color = Color::Rgb(20, 22, 26);
    pub const PANEL_ALT: Color = Color::Rgb(28, 31, 36);
    pub const PANEL_SOFT: Color = Color::Rgb(36, 40, 46);
    pub const EDITOR_PANE: Color = Color::Rgb(9, 12, 18);
    pub const EDITOR_PANE_FOCUSED: Color = Color::Rgb(14, 20, 34);
    pub const SPLIT_DIVIDER: Color = Color::Rgb(34, 40, 54);
    pub const NORMAL_MODE: Color = Color::Rgb(128, 143, 167);
    pub const INSERT_MODE: Color = Color::Rgb(162, 187, 152);
    pub const SHELL_MODE: Color = Color::Rgb(176, 154, 196);
    pub const SYNTAX_KEYWORD: Color = Color::Rgb(168, 149, 214);
    pub const SYNTAX_STRING: Color = Color::Rgb(156, 194, 140);
    pub const SYNTAX_COMMENT: Color = Color::Rgb(104, 111, 122);
    pub const SYNTAX_TYPE: Color = Color::Rgb(118, 170, 202);
    pub const SYNTAX_FUNCTION: Color = Color::Rgb(214, 186, 126);
    pub const SYNTAX_VARIABLE: Color = Color::Rgb(196, 198, 204);
    pub const SYNTAX_PARAMETER: Color = Color::Rgb(224, 177, 118);
    pub const SYNTAX_NUMBER: Color = Color::Rgb(210, 151, 116);
    pub const SYNTAX_OPERATOR: Color = Color::Rgb(148, 155, 168);
    pub const SYNTAX_MACRO: Color = Color::Rgb(199, 146, 234);
    pub const SYNTAX_NAMESPACE: Color = Color::Rgb(128, 162, 196);
    pub const SYNTAX_PROPERTY: Color = Color::Rgb(124, 193, 173);
}

use ratatui::style::Color;

pub struct AppColors;

impl AppColors {
    pub const BACKGROUND: Color = Color::Rgb(8, 8, 10);
    pub const FOREGROUND: Color = Color::Rgb(198, 204, 214);
    pub const MUTED: Color = Color::Rgb(132, 140, 152);
    pub const CURRENT_LINE_NUMBER: Color = Color::Rgb(236, 240, 246);
    pub const INDENT_GUIDE: Color = Color::Rgb(72, 78, 88);
    pub const ACCENT: Color = Color::Rgb(155, 183, 206);
    pub const SEARCH_HIGHLIGHT: Color = Color::Rgb(176, 148, 92);
    pub const SELECTION_HIGHLIGHT: Color = Color::Rgb(58, 78, 118);
    pub const WORD_HIGHLIGHT: Color = Color::Rgb(30, 41, 57);
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
    pub const NORMAL_MODE: Color = Color::Rgb(128, 146, 169);
    pub const INSERT_MODE: Color = Color::Rgb(152, 180, 150);
    pub const SHELL_MODE: Color = Color::Rgb(170, 150, 188);
    pub const SYNTAX_KEYWORD: Color = Color::Rgb(196, 162, 120);
    pub const SYNTAX_STRING: Color = Color::Rgb(146, 186, 150);
    pub const SYNTAX_COMMENT: Color = Color::Rgb(98, 106, 118);
    pub const SYNTAX_TYPE: Color = Color::Rgb(118, 176, 196);
    pub const SYNTAX_FUNCTION: Color = Color::Rgb(126, 164, 210);
    pub const SYNTAX_VARIABLE: Color = Color::Rgb(170, 206, 188);
    pub const SYNTAX_PARAMETER: Color = Color::Rgb(210, 170, 126);
    pub const SYNTAX_NUMBER: Color = Color::Rgb(214, 164, 122);
    pub const SYNTAX_OPERATOR: Color = Color::Rgb(142, 149, 162);
    pub const SYNTAX_MACRO: Color = Color::Rgb(178, 142, 198);
    pub const SYNTAX_NAMESPACE: Color = Color::Rgb(138, 164, 196);
    pub const SYNTAX_PROPERTY: Color = Color::Rgb(118, 190, 172);
}

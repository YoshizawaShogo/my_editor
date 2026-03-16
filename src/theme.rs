use clap::ValueEnum;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ThemeOption {
    Classic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    pub background: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub text: Color,
    pub text_muted: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
}

impl ThemePalette {
    pub fn resolve(option: ThemeOption) -> Self {
        match option {
            ThemeOption::Classic => ThemePalette {
                background: Color::Rgb(10, 10, 10),
                accent: Color::Rgb(186, 186, 186),
                accent_soft: Color::Rgb(74, 74, 74),
                text: Color::Rgb(224, 224, 224),
                text_muted: Color::Rgb(150, 150, 150),
                selection_fg: Color::Rgb(10, 10, 10),
                selection_bg: Color::Rgb(186, 186, 186),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_theme_uses_dark_grayscale_palette() {
        let palette = ThemePalette::resolve(ThemeOption::Classic);
        assert_eq!(palette.background, Color::Rgb(10, 10, 10));
        assert_eq!(palette.accent, Color::Rgb(186, 186, 186));
        assert_eq!(palette.selection_bg, Color::Rgb(186, 186, 186));
    }
}

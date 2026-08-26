//! Built-in, dependency-free terminal colour schemes.
//!
//! The renderer will consume the complete ANSI palette in v0.2. Keeping the
//! palette here already makes the preview and the future PTY renderer use the
//! same user-facing setting.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThemeAppearance {
    Dark,
    Light,
}

impl ThemeAppearance {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalTheme {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) appearance: ThemeAppearance,
    pub(crate) background: u32,
    pub(crate) foreground: u32,
    pub(crate) cursor: u32,
    pub(crate) selection: u32,
    /// ANSI colours in the order black, red, green, yellow, blue, magenta,
    /// cyan, white, then their bright equivalents.
    pub(crate) ansi: [u32; 16],
}

pub(crate) const STANDARD_TERMINAL_THEMES: [TerminalTheme; 10] = [
    TerminalTheme {
        id: "dracula",
        name: "Dracula",
        appearance: ThemeAppearance::Dark,
        background: 0x282a36,
        foreground: 0xf8f8f2,
        cursor: 0xf8f8f0,
        selection: 0x44475a,
        ansi: [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    },
    TerminalTheme {
        id: "one-dark",
        name: "One Dark",
        appearance: ThemeAppearance::Dark,
        background: 0x282c34,
        foreground: 0xabb2bf,
        cursor: 0x528bff,
        selection: 0x3e4452,
        ansi: [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
            0x5c6370, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
        ],
    },
    TerminalTheme {
        id: "nord",
        name: "Nord",
        appearance: ThemeAppearance::Dark,
        background: 0x2e3440,
        foreground: 0xd8dee9,
        cursor: 0xd8dee9,
        selection: 0x434c5e,
        ansi: [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    },
    TerminalTheme {
        id: "tokyo-night",
        name: "Tokyo Night",
        appearance: ThemeAppearance::Dark,
        background: 0x1a1b26,
        foreground: 0xc0caf5,
        cursor: 0xc0caf5,
        selection: 0x33467c,
        ansi: [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    },
    TerminalTheme {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        appearance: ThemeAppearance::Dark,
        background: 0x282828,
        foreground: 0xebdbb2,
        cursor: 0xebdbb2,
        selection: 0x504945,
        ansi: [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    },
    TerminalTheme {
        id: "catppuccin-mocha",
        name: "Catppuccin Mocha",
        appearance: ThemeAppearance::Dark,
        background: 0x1e1e2e,
        foreground: 0xcdd6f4,
        cursor: 0xf5e0dc,
        selection: 0x585b70,
        ansi: [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
    },
    TerminalTheme {
        id: "solarized-light",
        name: "Solarized Light",
        appearance: ThemeAppearance::Light,
        background: 0xfdf6e3,
        foreground: 0x657b83,
        cursor: 0x586e75,
        selection: 0xeee8d5,
        ansi: [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    },
    TerminalTheme {
        id: "one-light",
        name: "One Light",
        appearance: ThemeAppearance::Light,
        background: 0xfafafa,
        foreground: 0x383a42,
        cursor: 0x526eff,
        selection: 0xd3e3fd,
        ansi: [
            0x000000, 0xca1243, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7,
            0x696c77, 0xca1243, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xffffff,
        ],
    },
    TerminalTheme {
        id: "github-light",
        name: "GitHub Light",
        appearance: ThemeAppearance::Light,
        background: 0xffffff,
        foreground: 0x24292f,
        cursor: 0x0969da,
        selection: 0xb6e3ff,
        ansi: [
            0x24292f, 0xcf222e, 0x116329, 0x4d2d00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
            0x57606a, 0xa40e26, 0x1a7f37, 0x9a6700, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
    },
    TerminalTheme {
        id: "catppuccin-latte",
        name: "Catppuccin Latte",
        appearance: ThemeAppearance::Light,
        background: 0xeff1f5,
        foreground: 0x4c4f69,
        cursor: 0xdc8a78,
        selection: 0xacb0be,
        ansi: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
    },
];

pub(crate) fn theme_by_id(id: &str) -> Option<TerminalTheme> {
    let mut index = 0;
    while index < STANDARD_TERMINAL_THEMES.len() {
        let theme = STANDARD_TERMINAL_THEMES[index];
        if theme.id == id {
            return Some(theme);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{STANDARD_TERMINAL_THEMES, ThemeAppearance, theme_by_id};

    #[test]
    fn includes_dracula_and_both_appearances() {
        assert_eq!(theme_by_id("dracula").unwrap().name, "Dracula");
        assert!(
            STANDARD_TERMINAL_THEMES
                .iter()
                .any(|theme| theme.appearance == ThemeAppearance::Dark)
        );
        assert!(
            STANDARD_TERMINAL_THEMES
                .iter()
                .any(|theme| theme.appearance == ThemeAppearance::Light)
        );
    }

    #[test]
    fn every_builtin_theme_has_a_complete_ansi_palette() {
        for theme in STANDARD_TERMINAL_THEMES {
            assert_eq!(theme.ansi.len(), 16, "{}", theme.name);
            assert_ne!(theme.background, theme.foreground, "{}", theme.name);
        }
    }
}

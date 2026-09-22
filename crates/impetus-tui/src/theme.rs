//! Named TUI color themes.
//!
//! Default is the Impetus signature (`impetus` — neon / starfield). Geek pack
//! covers familiar dark palettes. Select with `/theme`, Ctrl+Shift+T (cycle),
//! or `IMPETUS_TUI_THEME=<id>`.

use ratatui::style::{Color, Modifier, Style};

use crate::model::ItemKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub border: Color,
    pub muted: Color,
    pub text: Color,
    pub accent: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeMeta {
    pub id: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
}

/// Catalog order = picker order. First entry is the product default.
pub const THEME_CATALOG: &[ThemeMeta] = &[
    ThemeMeta {
        id: "impetus",
        label: "Impetus Neon",
        blurb: "signature void + magenta thrust / cyan stars",
    },
    ThemeMeta {
        id: "impetus-stars",
        label: "Impetus Stars",
        blurb: "deeper black, soft starlight accents",
    },
    ThemeMeta {
        id: "dracula",
        label: "Dracula",
        blurb: "classic purple geek dark",
    },
    ThemeMeta {
        id: "nord",
        label: "Nord",
        blurb: "arctic polar night",
    },
    ThemeMeta {
        id: "gruvbox",
        label: "Gruvbox",
        blurb: "warm retro terminal",
    },
    ThemeMeta {
        id: "tokyo-night",
        label: "Tokyo Night",
        blurb: "cool city neon blue",
    },
    ThemeMeta {
        id: "catppuccin",
        label: "Catppuccin Mocha",
        blurb: "pastel mocha dark",
    },
    ThemeMeta {
        id: "solarized",
        label: "Solarized Dark",
        blurb: "Ethan Schoonover classic",
    },
    ThemeMeta {
        id: "monokai",
        label: "Monokai",
        blurb: "loud editor neon",
    },
    ThemeMeta {
        id: "one-dark",
        label: "One Dark",
        blurb: "Atom / VS Code familiar",
    },
    ThemeMeta {
        id: "matrix",
        label: "Matrix",
        blurb: "phosphor green on black",
    },
    ThemeMeta {
        id: "zinc",
        label: "Zinc",
        blurb: "neutral low-chroma dark",
    },
];

impl Default for Theme {
    fn default() -> Self {
        theme_by_id(DEFAULT_THEME_ID).unwrap_or_else(impetus_neon)
    }
}

pub const DEFAULT_THEME_ID: &str = "impetus";

/// Resolve theme id from `IMPETUS_TUI_THEME` or product default.
pub fn theme_id_from_env() -> &'static str {
    let Ok(raw) = std::env::var("IMPETUS_TUI_THEME") else {
        return DEFAULT_THEME_ID;
    };
    let needle = raw.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return DEFAULT_THEME_ID;
    }
    THEME_CATALOG
        .iter()
        .find(|meta| meta.id == needle || meta.label.eq_ignore_ascii_case(&needle))
        .map(|meta| meta.id)
        .unwrap_or(DEFAULT_THEME_ID)
}

pub fn theme_meta(id: &str) -> Option<&'static ThemeMeta> {
    let needle = id.trim().to_ascii_lowercase();
    THEME_CATALOG
        .iter()
        .find(|meta| meta.id == needle || meta.label.eq_ignore_ascii_case(&needle))
}

pub fn theme_by_id(id: &str) -> Option<Theme> {
    let meta = theme_meta(id)?;
    Some(match meta.id {
        "impetus" => impetus_neon(),
        "impetus-stars" => impetus_stars(),
        "dracula" => dracula(),
        "nord" => nord(),
        "gruvbox" => gruvbox(),
        "tokyo-night" => tokyo_night(),
        "catppuccin" => catppuccin_mocha(),
        "solarized" => solarized_dark(),
        "monokai" => monokai(),
        "one-dark" => one_dark(),
        "matrix" => matrix(),
        "zinc" => zinc(),
        _ => return None,
    })
}

pub fn resolve_theme(id: &str) -> Theme {
    theme_by_id(id).unwrap_or_else(impetus_neon)
}

pub fn cycle_theme_id(current: &str) -> &'static str {
    let idx = THEME_CATALOG
        .iter()
        .position(|meta| meta.id == current)
        .unwrap_or(0);
    THEME_CATALOG[(idx + 1) % THEME_CATALOG.len()].id
}

pub fn theme_index(id: &str) -> usize {
    THEME_CATALOG
        .iter()
        .position(|meta| meta.id == id)
        .unwrap_or(0)
}

impl Theme {
    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }

    pub fn panel(self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    pub fn selected(self) -> Style {
        Style::default()
            .fg(self.text)
            .bg(self.surface_alt)
            .add_modifier(Modifier::BOLD)
    }

    pub fn item_color(self, kind: ItemKind) -> Color {
        match kind {
            ItemKind::User => self.blue,
            ItemKind::Assistant => self.accent,
            ItemKind::Plan => self.cyan,
            ItemKind::Tool | ItemKind::Activity => self.green,
            ItemKind::Approval => self.yellow,
            ItemKind::Notice => self.muted,
            ItemKind::Error => self.red,
            ItemKind::Budget => self.magenta,
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Signature Impetus: deep space void, magenta thrust, cyan star flecks.
fn impetus_neon() -> Theme {
    Theme {
        background: rgb(6, 8, 18),
        surface: rgb(12, 16, 32),
        surface_alt: rgb(22, 18, 48),
        border: rgb(72, 48, 120),
        muted: rgb(140, 148, 180),
        text: rgb(236, 240, 255),
        accent: rgb(255, 46, 196),
        green: rgb(80, 250, 180),
        yellow: rgb(255, 214, 102),
        red: rgb(255, 85, 140),
        blue: rgb(100, 180, 255),
        magenta: rgb(210, 120, 255),
        cyan: rgb(64, 224, 255),
    }
}

/// Soft starfield: cooler blacks, dimmer nebula, bright star cyan.
fn impetus_stars() -> Theme {
    Theme {
        background: rgb(3, 4, 12),
        surface: rgb(10, 12, 24),
        surface_alt: rgb(18, 22, 40),
        border: rgb(48, 64, 96),
        muted: rgb(120, 132, 160),
        text: rgb(220, 228, 245),
        accent: rgb(180, 160, 255),
        green: rgb(120, 220, 180),
        yellow: rgb(240, 220, 160),
        red: rgb(255, 120, 140),
        blue: rgb(120, 160, 255),
        magenta: rgb(200, 140, 255),
        cyan: rgb(160, 230, 255),
    }
}

fn dracula() -> Theme {
    Theme {
        background: rgb(40, 42, 54),
        surface: rgb(52, 55, 70),
        surface_alt: rgb(68, 71, 90),
        border: rgb(98, 114, 164),
        muted: rgb(152, 159, 177),
        text: rgb(248, 248, 242),
        accent: rgb(189, 147, 249),
        green: rgb(80, 250, 123),
        yellow: rgb(241, 250, 140),
        red: rgb(255, 85, 85),
        blue: rgb(139, 233, 253),
        magenta: rgb(255, 121, 198),
        cyan: rgb(139, 233, 253),
    }
}

fn nord() -> Theme {
    Theme {
        background: rgb(46, 52, 64),
        surface: rgb(59, 66, 82),
        surface_alt: rgb(67, 76, 94),
        border: rgb(76, 86, 106),
        muted: rgb(129, 161, 193),
        text: rgb(236, 239, 244),
        accent: rgb(136, 192, 208),
        green: rgb(163, 190, 140),
        yellow: rgb(235, 203, 139),
        red: rgb(191, 97, 106),
        blue: rgb(129, 161, 193),
        magenta: rgb(180, 142, 173),
        cyan: rgb(143, 188, 187),
    }
}

fn gruvbox() -> Theme {
    Theme {
        background: rgb(40, 40, 40),
        surface: rgb(60, 56, 54),
        surface_alt: rgb(80, 73, 69),
        border: rgb(124, 111, 100),
        muted: rgb(168, 153, 132),
        text: rgb(235, 219, 178),
        accent: rgb(254, 128, 25),
        green: rgb(184, 187, 38),
        yellow: rgb(250, 189, 47),
        red: rgb(251, 73, 52),
        blue: rgb(131, 165, 152),
        magenta: rgb(211, 134, 155),
        cyan: rgb(142, 192, 124),
    }
}

fn tokyo_night() -> Theme {
    Theme {
        background: rgb(26, 27, 38),
        surface: rgb(36, 40, 59),
        surface_alt: rgb(41, 46, 66),
        border: rgb(65, 72, 104),
        muted: rgb(120, 130, 170),
        text: rgb(192, 202, 245),
        accent: rgb(122, 162, 247),
        green: rgb(158, 206, 106),
        yellow: rgb(224, 175, 104),
        red: rgb(247, 118, 142),
        blue: rgb(125, 207, 255),
        magenta: rgb(187, 154, 247),
        cyan: rgb(125, 207, 255),
    }
}

fn catppuccin_mocha() -> Theme {
    Theme {
        background: rgb(30, 30, 46),
        surface: rgb(49, 50, 68),
        surface_alt: rgb(69, 71, 90),
        border: rgb(88, 91, 112),
        muted: rgb(166, 173, 200),
        text: rgb(205, 214, 244),
        accent: rgb(203, 166, 247),
        green: rgb(166, 227, 161),
        yellow: rgb(249, 226, 175),
        red: rgb(243, 139, 168),
        blue: rgb(137, 180, 250),
        magenta: rgb(245, 194, 231),
        cyan: rgb(148, 226, 213),
    }
}

fn solarized_dark() -> Theme {
    Theme {
        background: rgb(0, 43, 54),
        surface: rgb(7, 54, 66),
        surface_alt: rgb(0, 61, 73),
        border: rgb(88, 110, 117),
        muted: rgb(131, 148, 150),
        text: rgb(238, 232, 213),
        accent: rgb(38, 139, 210),
        green: rgb(133, 153, 0),
        yellow: rgb(181, 137, 0),
        red: rgb(220, 50, 47),
        blue: rgb(38, 139, 210),
        magenta: rgb(211, 54, 130),
        cyan: rgb(42, 161, 152),
    }
}

fn monokai() -> Theme {
    Theme {
        background: rgb(39, 40, 34),
        surface: rgb(50, 51, 44),
        surface_alt: rgb(73, 72, 62),
        border: rgb(117, 113, 94),
        muted: rgb(117, 113, 94),
        text: rgb(248, 248, 242),
        accent: rgb(174, 129, 255),
        green: rgb(166, 226, 46),
        yellow: rgb(230, 219, 116),
        red: rgb(249, 38, 114),
        blue: rgb(102, 217, 239),
        magenta: rgb(249, 38, 114),
        cyan: rgb(161, 239, 228),
    }
}

fn one_dark() -> Theme {
    Theme {
        background: rgb(40, 44, 52),
        surface: rgb(49, 54, 63),
        surface_alt: rgb(57, 63, 74),
        border: rgb(76, 82, 99),
        muted: rgb(171, 178, 191),
        text: rgb(171, 178, 191),
        accent: rgb(198, 120, 221),
        green: rgb(152, 195, 121),
        yellow: rgb(229, 192, 123),
        red: rgb(224, 108, 117),
        blue: rgb(97, 175, 239),
        magenta: rgb(198, 120, 221),
        cyan: rgb(86, 182, 194),
    }
}

fn matrix() -> Theme {
    Theme {
        background: rgb(0, 8, 0),
        surface: rgb(0, 18, 0),
        surface_alt: rgb(0, 32, 0),
        border: rgb(0, 80, 20),
        muted: rgb(40, 120, 50),
        text: rgb(160, 255, 170),
        accent: rgb(0, 255, 70),
        green: rgb(0, 220, 60),
        yellow: rgb(180, 255, 80),
        red: rgb(255, 60, 60),
        blue: rgb(80, 200, 120),
        magenta: rgb(120, 255, 160),
        cyan: rgb(100, 255, 180),
    }
}

fn zinc() -> Theme {
    Theme {
        background: rgb(9, 9, 11),
        surface: rgb(24, 24, 27),
        surface_alt: rgb(39, 39, 42),
        border: rgb(63, 63, 70),
        muted: rgb(161, 161, 170),
        text: rgb(244, 244, 245),
        accent: rgb(161, 161, 170),
        green: rgb(134, 239, 172),
        yellow: rgb(253, 224, 71),
        red: rgb(252, 165, 165),
        blue: rgb(147, 197, 253),
        magenta: rgb(216, 180, 254),
        cyan: rgb(103, 232, 249),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_resolvable() {
        let mut seen = std::collections::BTreeSet::new();
        for meta in THEME_CATALOG {
            assert!(seen.insert(meta.id), "duplicate theme id {}", meta.id);
            let theme = theme_by_id(meta.id).expect("palette");
            assert_ne!(theme.background, theme.text);
            assert_ne!(theme.accent, theme.background);
        }
    }

    #[test]
    fn default_is_impetus_neon() {
        assert_eq!(DEFAULT_THEME_ID, "impetus");
        assert_eq!(Theme::default(), impetus_neon());
    }

    #[test]
    fn cycle_wraps_catalog() {
        let mut id = DEFAULT_THEME_ID;
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..THEME_CATALOG.len() {
            assert!(visited.insert(id));
            id = cycle_theme_id(id);
        }
        assert_eq!(id, DEFAULT_THEME_ID);
        assert_eq!(visited.len(), THEME_CATALOG.len());
    }

    #[test]
    fn resolve_accepts_label_case_insensitive() {
        assert!(theme_by_id("Nord").is_some());
        assert!(theme_by_id("IMPETUS-STARS").is_some());
        assert!(theme_meta("tokyo-night").is_some());
        assert!(theme_meta("Tokyo Night").is_some());
        assert!(theme_meta("not-a-theme").is_none());
    }

    #[test]
    fn unknown_falls_back_to_impetus() {
        assert_eq!(resolve_theme("nope"), impetus_neon());
    }
}

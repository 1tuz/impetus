use ratatui::style::{Color, Modifier, Style};

use crate::model::ItemKind;

#[derive(Clone, Copy, Debug)]
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

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::Rgb(11, 14, 20),
            surface: Color::Rgb(18, 23, 31),
            surface_alt: Color::Rgb(25, 31, 41),
            border: Color::Rgb(58, 68, 84),
            muted: Color::Rgb(126, 137, 154),
            text: Color::Rgb(225, 230, 238),
            accent: Color::Rgb(154, 127, 255),
            green: Color::Rgb(110, 214, 151),
            yellow: Color::Rgb(255, 208, 117),
            red: Color::Rgb(255, 111, 124),
            blue: Color::Rgb(103, 183, 255),
            magenta: Color::Rgb(229, 139, 255),
            cyan: Color::Rgb(98, 214, 220),
        }
    }
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
            ItemKind::Tool => self.green,
            ItemKind::Approval => self.yellow,
            ItemKind::Notice => self.muted,
            ItemKind::Error => self.red,
            ItemKind::Budget => self.magenta,
        }
    }
}

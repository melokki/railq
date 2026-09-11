//! Semantic styles for the RailQ control-room shell.

//! The palette is deliberately named by role so later views can share the
//! control-room language without coupling presentation to game state.

use ratatui::{
    style::{Color, Style},
    widgets::Borders,
};

pub const BACKGROUND: Color = Color::Rgb(16, 24, 32);
pub const PANEL: Color = Color::Rgb(24, 38, 48);
pub const PRIMARY: Color = Color::Rgb(230, 237, 243);
pub const SECONDARY: Color = Color::Rgb(164, 179, 192);
pub const ACCENT: Color = Color::Rgb(98, 214, 232);
pub const WARNING: Color = Color::Rgb(240, 189, 101);
pub const SUCCESS: Color = Color::Rgb(139, 212, 156);
pub const ERROR: Color = Color::Rgb(240, 128, 128);

pub const THIN_BORDERS: Borders = Borders::ALL;

pub fn terminal() -> Style {
    Style::default().bg(BACKGROUND).fg(PRIMARY)
}

pub fn panel() -> Style {
    Style::default().bg(PANEL).fg(PRIMARY)
}

pub fn border() -> Style {
    Style::default().fg(SECONDARY).bg(PANEL)
}

pub fn title() -> Style {
    Style::default().fg(PRIMARY).bg(PANEL).bold()
}

pub fn secondary() -> Style {
    Style::default().fg(SECONDARY).bg(PANEL)
}

pub fn focused_border() -> Style {
    Style::default().fg(ACCENT).bg(PANEL)
}

pub fn focused_title() -> Style {
    Style::default().fg(ACCENT).bg(PANEL).bold()
}

pub fn table_header() -> Style {
    Style::default().fg(SECONDARY).bg(PANEL).bold()
}

pub fn selected_row() -> Style {
    Style::default().fg(BACKGROUND).bg(ACCENT).bold()
}

pub fn active_tab() -> Style {
    Style::default().fg(ACCENT).bg(PANEL).bold()
}

pub fn hint() -> Style {
    Style::default().fg(SECONDARY).bg(BACKGROUND)
}

pub fn feedback() -> Style {
    Style::default().fg(WARNING).bg(BACKGROUND)
}

pub fn error() -> Style {
    Style::default().fg(ERROR).bg(PANEL)
}

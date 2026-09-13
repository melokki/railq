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

/// Muted foreground used for application content behind a focused modal.
pub const MODAL_BACKDROP_TEXT: Color = Color::Rgb(84, 100, 112);
/// Dark scrim-like background used while a focused modal owns input.
pub const MODAL_BACKDROP: Color = Color::Rgb(12, 19, 25);

pub const THIN_BORDERS: Borders = Borders::ALL;
pub const SELECTION_MARKER: &str = "> ";

pub fn terminal() -> Style {
    Style::default().bg(BACKGROUND).fg(PRIMARY)
}

/// Re-styles an already rendered application layer as a muted modal backdrop.
/// `Style::reset` intentionally removes bold/selection treatment so bright
/// rows and tabs cannot compete with the active dialog.
pub fn modal_backdrop() -> Style {
    Style::reset().fg(MODAL_BACKDROP_TEXT).bg(MODAL_BACKDROP)
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

pub fn primary_value() -> Style {
    Style::default().fg(PRIMARY).bg(PANEL)
}

pub fn success() -> Style {
    Style::default().fg(SUCCESS).bg(PANEL)
}

pub fn warning() -> Style {
    Style::default().fg(WARNING).bg(PANEL)
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

pub fn navigation() -> Style {
    Style::default().fg(SECONDARY).bg(BACKGROUND)
}

pub fn active_tab() -> Style {
    Style::default().fg(BACKGROUND).bg(ACCENT).bold()
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

/// Accent treatment for shortcut keycaps in the global action bar.
pub fn shortcut_key() -> Style {
    Style::default().fg(ACCENT).bg(PANEL).bold()
}

/// Primary treatment for the action name which follows a shortcut keycap.
pub fn shortcut_action() -> Style {
    Style::default().fg(PRIMARY).bg(PANEL)
}

/// Muted treatment for a shortcut which is currently unavailable.
pub fn shortcut_disabled() -> Style {
    Style::default().fg(SECONDARY).bg(PANEL)
}

/// The footer is a first-class control surface, so its frame uses the accent.
pub fn footer_border() -> Style {
    Style::default().fg(ACCENT).bg(PANEL)
}

//! Reusable visual primitives shared by RailQ workspaces.
//!
//! Keep these deliberately small: they define presentation language without
//! owning workspace state or game behaviour.

use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::Block,
};

use crate::ui::theme;

/// Standard bordered RailQ panel with optional focus treatment.
pub fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(theme::THIN_BORDERS)
        .border_style(if focused {
            theme::focused_border()
        } else {
            theme::border()
        })
        .title(title)
        .title_style(if focused {
            theme::focused_title()
        } else {
            theme::title()
        })
        .style(theme::panel())
}

/// Standard section heading used inside inspectors and detail surfaces.
pub fn section_heading(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::table_header())
}

/// Standard two-column inspector line with a muted label.
pub fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<18}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
}

/// Standard two-column inspector line with a caller-provided value style.
pub fn labelled_line_styled(label: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<18}"), theme::secondary()),
        Span::styled(value.to_owned(), style),
    ])
}

//! Reusable visual primitives shared by RailQ workspaces.
//!
//! Keep these deliberately small: they define presentation language without
//! owning workspace state or game behaviour.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

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

const EMPTY_STATE_MAX_WIDTH: u16 = 64;
const EMPTY_STATE_HORIZONTAL_MARGIN: u16 = 2;

/// Primary action shown by an empty workspace state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyStateAction<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

impl<'a> EmptyStateAction<'a> {
    pub const fn new(key: &'a str, label: &'a str) -> Self {
        Self { key, label }
    }
}

/// Shared RailQ empty-state presentation.
///
/// Empty workspaces use one quiet hierarchy: an optional Unicode motif, a
/// concise title, one explanatory sentence, an optional primary action, and
/// one optional hint. The workspace itself owns the surrounding panel; this
/// primitive deliberately avoids drawing a second card or border inside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyState<'a> {
    pub motif: Option<&'a str>,
    pub title: &'a str,
    pub description: &'a str,
    pub primary_action: Option<EmptyStateAction<'a>>,
    pub hint: Option<&'a str>,
}

impl<'a> EmptyState<'a> {
    pub const fn new(title: &'a str, description: &'a str) -> Self {
        Self {
            motif: None,
            title,
            description,
            primary_action: None,
            hint: None,
        }
    }

    pub const fn motif(mut self, motif: &'a str) -> Self {
        self.motif = Some(motif);
        self
    }

    pub const fn primary_action(mut self, key: &'a str, label: &'a str) -> Self {
        self.primary_action = Some(EmptyStateAction::new(key, label));
        self
    }

    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// Renders the empty state centered inside an existing workspace panel.
    pub fn render(self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let width = empty_state_width(area.width);
        let lines = self.lines(width);
        let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX).min(area.height);
        let render_area = Rect {
            x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
            y: area
                .y
                .saturating_add(area.height.saturating_sub(content_height) / 2),
            width,
            height: content_height,
        };

        frame.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .style(theme::panel()),
            render_area,
        );
    }

    fn lines(self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        if let Some(motif) = self.motif.filter(|motif| !motif.trim().is_empty()) {
            lines.push(Line::styled(motif.to_owned(), theme::empty_state_motif()));
            lines.push(Line::from(""));
        }

        lines.push(Line::styled(
            self.title.to_owned(),
            theme::empty_state_title(),
        ));

        if !self.description.trim().is_empty() {
            lines.push(Line::from(""));
            lines.extend(
                wrap_empty_state_text(self.description, usize::from(width))
                    .into_iter()
                    .map(|line| Line::styled(line, theme::empty_state_description())),
            );
        }

        if let Some(action) = self.primary_action {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(format!("[{}]", action.key), theme::shortcut_key()),
                Span::styled(format!(" {}", action.label), theme::shortcut_action()),
            ]));
        }

        if let Some(hint) = self.hint.filter(|hint| !hint.trim().is_empty()) {
            lines.push(Line::from(""));
            lines.extend(
                wrap_empty_state_text(hint, usize::from(width))
                    .into_iter()
                    .map(|line| Line::styled(line, theme::empty_state_hint())),
            );
        }

        lines
    }
}

fn empty_state_width(area_width: u16) -> u16 {
    if area_width <= EMPTY_STATE_HORIZONTAL_MARGIN.saturating_mul(2) {
        return area_width;
    }

    area_width
        .saturating_sub(EMPTY_STATE_HORIZONTAL_MARGIN.saturating_mul(2))
        .min(EMPTY_STATE_MAX_WIDTH)
}

fn wrap_empty_state_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for paragraph in text.lines() {
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let separator_width = if current.is_empty() { 0 } else { 1 };
            let candidate_width = UnicodeWidthStr::width(current.as_str())
                .saturating_add(separator_width)
                .saturating_add(UnicodeWidthStr::width(word));

            if !current.is_empty() && candidate_width > width {
                lines.push(current);
                current = word.to_owned();
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    lines
}

#[cfg(test)]
mod empty_state_tests {
    use super::{EmptyState, empty_state_width, wrap_empty_state_text};

    fn line_text(line: &ratatui::text::Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn empty_state_keeps_the_shared_visual_hierarchy() {
        let state = EmptyState::new("No Passenger Services", "Create a reusable route.")
            .motif("●━━●━━●")
            .primary_action("N", "Create service")
            .hint("Services operate both directions by default.");

        let rendered = state
            .lines(60)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![
                "●━━●━━●",
                "",
                "No Passenger Services",
                "",
                "Create a reusable route.",
                "",
                "[N] Create service",
                "",
                "Services operate both directions by default.",
            ]
        );
    }

    #[test]
    fn optional_sections_do_not_leave_extra_spacing() {
        let rendered = EmptyState::new("Nothing here", "")
            .lines(40)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["Nothing here"]);
    }

    #[test]
    fn description_and_hint_wrap_on_word_boundaries() {
        assert_eq!(
            wrap_empty_state_text("Create a reusable passenger route", 18),
            vec!["Create a reusable", "passenger route"]
        );
    }

    #[test]
    fn empty_state_width_keeps_margins_and_caps_long_lines() {
        assert_eq!(empty_state_width(100), 64);
        assert_eq!(empty_state_width(40), 36);
        assert_eq!(empty_state_width(3), 3);
    }
}

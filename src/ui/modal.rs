//! Shared modal chrome for focused keyboard workflows.
//!
//! The shell intentionally mirrors the interaction grammar used by Bastion:
//! one active border, padded content, a divider, and a dedicated shortcut bar.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::ui::theme;

const HORIZONTAL_PADDING: u16 = 1;

/// Content rectangles inside one modal window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModalAreas {
    pub body: Rect,
    pub footer: Rect,
}

/// Returns a centered rectangle capped to the available workspace.
///
/// Confirmation dialogs use this instead of taking over the whole workspace,
/// while still degrading cleanly to the full area on small terminals.
pub fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width);
    let height = area.height.min(max_height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    }
}

/// Standard geometry for multi-step workflows such as Manual Dispatch.
///
/// Keeping this in the shared modal module prevents individual workspaces from
/// drifting into slightly different centering and size rules.
pub fn workflow_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(6).min(90).max(44).min(area.width);
    let height = area
        .height
        .saturating_sub(2)
        .min(24)
        .max(14)
        .min(area.height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    }
}

/// Standard geometry for compact editors such as Train rename.
pub fn editor_rect(area: Rect, height: u16) -> Rect {
    centered_rect(area, 68, height)
}

/// Standard geometry for consequential yes/no style confirmations.
pub fn confirmation_rect(area: Rect, height: u16) -> Rect {
    centered_rect(area, 68, height)
}

/// Dims an already rendered application layer before drawing a focused modal.
///
/// Terminal UIs have no alpha channel, so the Bastion-style backdrop is
/// achieved by re-styling the existing cells while preserving their symbols.
/// The modal is rendered afterwards with the normal RailQ palette.
pub fn dim_backdrop(frame: &mut Frame, area: Rect) {
    frame.buffer_mut().set_style(area, theme::modal_backdrop());
}

/// Draws the shared RailQ modal shell and returns the padded body/footer areas.
pub fn render_shell(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    footer: Line<'static>,
) -> ModalAreas {
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::focused_border())
        .title(title)
        .title_style(theme::focused_title())
        .style(theme::panel());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [body_area, separator_area, footer_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    render_horizontal_separator(frame, separator_area);
    frame.render_widget(
        Paragraph::new(footer).style(theme::panel()),
        padded(footer_area, HORIZONTAL_PADDING, 0),
    );

    ModalAreas {
        body: padded(body_area, HORIZONTAL_PADDING, 0),
        footer: padded(footer_area, HORIZONTAL_PADDING, 0),
    }
}

/// Bastion-inspired keyboard shortcut line used inside focused modal windows.
///
/// Modal footers follow one interaction grammar everywhere in RailQ:
/// Esc first, Enter second, movement/navigation next, then contextual actions.
/// Destructive secondary actions are kept last. Call sites may provide shortcuts
/// in whatever order is most convenient; this renderer owns the convention.
pub fn shortcut_line(shortcuts: &[(&str, &str)]) -> Line<'static> {
    let mut ordered = shortcuts.iter().copied().enumerate().collect::<Vec<_>>();
    ordered.sort_by_key(|(original_index, (key, action))| {
        (shortcut_priority(key, action), *original_index)
    });

    let mut spans = Vec::new();
    for (index, (_, (key, action))) in ordered.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", theme::shortcut_action()));
        }
        spans.push(Span::styled(format!("[{key}]"), theme::shortcut_key()));
        spans.push(Span::styled(format!(" {action}"), theme::shortcut_action()));
    }
    Line::from(spans)
}

fn shortcut_priority(key: &str, action: &str) -> u8 {
    let normalized_key = key.to_ascii_lowercase();
    let normalized_action = action.to_ascii_lowercase();

    if normalized_key.contains("esc") {
        return 0;
    }
    if normalized_key.contains("enter") {
        return 1;
    }
    if is_movement_shortcut(&normalized_key) {
        return 2;
    }
    if is_destructive_shortcut(&normalized_key, &normalized_action) {
        return 4;
    }
    3
}

fn is_movement_shortcut(key: &str) -> bool {
    key.contains('↑')
        || key.contains('↓')
        || key.contains("jk")
        || key.contains("pgup")
        || key.contains("pgdn")
        || (key.contains('←') && key.contains('→'))
}

fn is_destructive_shortcut(key: &str, action: &str) -> bool {
    key.contains("del")
        || ["delete", "remove", "unassign", "reset", "resell"]
            .iter()
            .any(|verb| action.contains(*verb))
}

#[cfg(test)]
mod tests {
    use super::shortcut_line;

    fn text(line: ratatui::text::Line<'static>) -> String {
        line.spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect()
    }

    #[test]
    fn modal_shortcuts_follow_the_global_footer_order() {
        let rendered = text(shortcut_line(&[
            ("Space", "toggle"),
            ("↑↓/JK", "choose"),
            ("Enter", "review"),
            ("Esc", "cancel"),
            ("Del", "delete"),
        ]));

        assert_eq!(
            rendered,
            "[Esc] cancel   [Enter] review   [↑↓/JK] choose   [Space] toggle   [Del] delete"
        );
    }

    #[test]
    fn escape_aliases_still_sort_first() {
        let rendered = text(shortcut_line(&[
            ("PgUp/PgDn", "page"),
            ("Q", "quit"),
            ("Esc/?", "close"),
        ]));

        assert_eq!(rendered, "[Esc/?] close   [PgUp/PgDn] page   [Q] quit");
    }
}

/// Draws a subtle vertical divider for picker/detail layouts.
pub fn render_vertical_separator(frame: &mut Frame, area: Rect) {
    let lines = (0..area.height)
        .map(|_| Line::styled("│", theme::secondary()))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).style(theme::panel()), area);
}

/// Draws a subtle horizontal divider for stacked modal sections.
pub fn render_horizontal_separator(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width))).style(theme::secondary()),
        area,
    );
}

fn padded(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    let x_padding = horizontal.min(area.width / 2);
    let y_padding = vertical.min(area.height / 2);
    Rect {
        x: area.x.saturating_add(x_padding),
        y: area.y.saturating_add(y_padding),
        width: area.width.saturating_sub(x_padding.saturating_mul(2)),
        height: area.height.saturating_sub(y_padding.saturating_mul(2)),
    }
}

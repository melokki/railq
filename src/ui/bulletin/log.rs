//! Bulletin development list rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table, Wrap},
};

use crate::{
    model::{GameState, UtcSeconds},
    ui::{components::section_heading, theme},
};

use super::{
    BulletinWorkspace,
    format::{category_label, category_style, relative_time},
    visible_entries,
};

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &mut BulletinWorkspace,
) {
    let [heading_area, body_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading("DEVELOPMENT LOG")).style(theme::panel()),
        heading_area,
    );

    let entries = visible_entries(state, workspace.filter());
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                ratatui::text::Line::styled(
                    "No Bulletin items in this category yet.",
                    theme::secondary(),
                ),
                ratatui::text::Line::styled(
                    "Significant council, Authority, construction, and network events will appear here.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            body_area,
        );
        workspace.table_state_mut().select(None);
        return;
    }

    workspace.set_page_size(usize::from(body_area.height.saturating_sub(1)).max(1));
    workspace.synchronize(state);
    let rows = entries
        .iter()
        .map(|(_, entry)| {
            Row::new(vec![
                Cell::from(relative_time(entry.occurred_at, now)),
                Cell::from(category_label(entry.category)).style(category_style(entry.category)),
                Cell::from(entry.headline.clone()),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Fill(1),
        ],
    )
    .header(Row::new(["When", "Type", "Development"]).style(theme::table_header()))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol(theme::SELECTION_MARKER)
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, body_area, workspace.table_state_mut());
}

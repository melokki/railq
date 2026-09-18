//! Selected Bulletin development rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
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
        Paragraph::new(section_heading("DEVELOPMENT")).style(theme::panel()),
        heading_area,
    );

    workspace.synchronize(state);
    let entries = visible_entries(state, workspace.filter());
    let selected = workspace
        .table_state_mut()
        .selected()
        .and_then(|index| entries.get(index));
    let Some((_, entry)) = selected else {
        frame.render_widget(
            Paragraph::new("No development selected.").style(theme::secondary()),
            body_area,
        );
        return;
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                category_label(entry.category),
                category_style(entry.category),
            ),
            Span::styled(
                format!("  ·  {}", relative_time(entry.occurred_at, now)),
                theme::secondary(),
            ),
        ]),
        Line::from(""),
        Line::styled(entry.headline.clone(), theme::title()),
        Line::from(""),
        Line::styled(entry.detail.clone(), theme::primary_value()),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        body_area,
    );
}

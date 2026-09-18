//! Selected Bulletin development rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    model::{BulletinEntry, GameState, UtcSeconds},
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
        Paragraph::new(section_heading("SELECTED DEVELOPMENT")).style(theme::panel()),
        heading_area,
    );

    workspace.synchronize(state);
    let entries = visible_entries(state, workspace.filter());
    let selected = workspace
        .table_state_mut()
        .selected()
        .and_then(|index| entries.get(index));
    let Some(&(bulletin_index, entry)) = selected else {
        render_empty_state(frame, body_area, state, workspace);
        return;
    };

    frame.render_widget(
        Paragraph::new(article_lines(
            entry,
            bulletin_index,
            workspace.visit_seen_count(state),
            now,
        ))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        body_area,
    );
}

pub(super) fn render_tiny(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &mut BulletinWorkspace,
) {
    workspace.synchronize(state);
    let entries = visible_entries(state, workspace.filter());
    workspace.set_page_size(1);

    let Some(selected_index) = workspace.table_state_mut().selected() else {
        render_empty_state(frame, area, state, workspace);
        return;
    };
    let Some(&(bulletin_index, entry)) = entries.get(selected_index) else {
        render_empty_state(frame, area, state, workspace);
        return;
    };

    let mut lines = vec![Line::from(vec![
        Span::styled("DEVELOPMENT  ", theme::table_header()),
        Span::styled(
            format!("{} / {}", selected_index + 1, entries.len()),
            theme::primary_value(),
        ),
        Span::styled(
            format!("  ·  {}", workspace.filter_label()),
            theme::secondary(),
        ),
    ])];
    lines.extend(article_lines(
        entry,
        bulletin_index,
        workspace.visit_seen_count(state),
        now,
    ));

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn article_lines(
    entry: &BulletinEntry,
    bulletin_index: usize,
    seen_count: u64,
    now: UtcSeconds,
) -> Vec<Line<'static>> {
    let is_new = entry_is_new(bulletin_index, seen_count);
    let mut metadata = vec![
        Span::styled(category_label(entry.category), category_style(entry.category)),
        Span::styled(
            format!("  ·  {}", relative_time(entry.occurred_at, now)),
            theme::secondary(),
        ),
    ];
    if is_new {
        metadata.push(Span::styled("  ·  NEW", theme::success().bold()));
    }

    vec![
        Line::from(metadata),
        Line::from(""),
        Line::styled(entry.headline.clone(), theme::title()),
        Line::from(""),
        Line::styled(entry.detail.clone(), theme::primary_value()),
    ]
}

fn entry_is_new(bulletin_index: usize, seen_count: u64) -> bool {
    u64::try_from(bulletin_index).unwrap_or(u64::MAX) >= seen_count
}

fn render_empty_state(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    workspace: &BulletinWorkspace,
) {
    let lines = if state.region.bulletin.is_empty() {
        vec![
            Line::styled("No developments recorded yet.", theme::secondary()),
            Line::styled(
                "Significant railway and Authority events will appear here.",
                theme::secondary(),
            ),
        ]
    } else {
        vec![
            Line::styled(
                format!("No developments in the {} view.", workspace.filter_label()),
                theme::secondary(),
            ),
            Line::styled("Press F to change the Bulletin view.", theme::hint()),
        ]
    };

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::entry_is_new;

    #[test]
    fn visit_boundary_marks_only_unseen_entries_as_new() {
        assert!(entry_is_new(4, 4));
        assert!(entry_is_new(5, 4));
        assert!(!entry_is_new(3, 4));
    }
}

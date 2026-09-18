//! Unified Bulletin workspace shell and responsive composition.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    model::{GameState, UtcSeconds},
    ui::{components::panel_block, theme},
};

use super::{
    BulletinWorkspace, detail,
    format::relative_time,
    layout::{BulletinLayout, compact_areas, wide_areas},
    log, visible_entries,
};

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &mut BulletinWorkspace,
) {
    let shell = panel_block("Bulletin", true);
    let inner = shell.inner(area);
    frame.render_widget(shell, area);

    match BulletinLayout::from_rect(area) {
        BulletinLayout::Wide => {
            let areas = wide_areas(inner);
            render_summary(frame, areas.summary, state, now, workspace);
            log::render(frame, areas.log, state, now, workspace);
            detail::render(frame, areas.detail, state, now, workspace);
        }
        BulletinLayout::Compact => {
            let areas = compact_areas(inner);
            render_summary(frame, areas.summary, state, now, workspace);
            log::render(frame, areas.log, state, now, workspace);
            detail::render(frame, areas.detail, state, now, workspace);
        }
        BulletinLayout::Tiny => {
            frame.render_widget(
                Paragraph::new(
                    "Railway Bulletin\n\nOpen this workspace in a larger terminal to inspect regional developments.",
                )
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
    }
}

fn render_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &BulletinWorkspace,
) {
    let entries = visible_entries(state, workspace.filter());
    let visible = entries.len();
    let total = state.region.bulletin.len();
    let latest = entries
        .first()
        .map(|(_, entry)| relative_time(entry.occurred_at, now))
        .unwrap_or_else(|| "—".into());
    let lines = vec![
        Line::from(vec![
            Span::styled("History  ", theme::secondary()),
            Span::styled(format!("{total} recorded"), theme::primary_value()),
            Span::styled(format!(" · {visible} shown"), theme::secondary()),
        ]),
        Line::from(vec![
            Span::styled("View  ", theme::secondary()),
            Span::styled(workspace.filter_label(), theme::focused_title()),
            Span::styled(" · Latest  ", theme::secondary()),
            Span::styled(latest, theme::primary_value()),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).style(theme::panel()), area);
}

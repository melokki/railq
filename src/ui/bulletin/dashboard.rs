//! Unified Bulletin workspace shell and responsive composition.

use ratatui::{Frame, layout::Rect};

use crate::{
    model::{GameState, UtcSeconds},
    ui::components::panel_block,
};

use super::{
    BulletinWorkspace, detail, filter_bar, header,
    layout::{BulletinLayout, compact_areas, wide_areas},
    log,
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
            header::render(frame, areas.briefing, state, now, workspace);
            filter_bar::render(frame, areas.filters, state, workspace);
            log::render(frame, areas.log, state, now, workspace);
            detail::render(frame, areas.detail, state, now, workspace);
        }
        BulletinLayout::Compact => {
            let areas = compact_areas(inner);
            header::render_compact(frame, areas.briefing, state, workspace);
            filter_bar::render(frame, areas.filters, state, workspace);
            log::render(frame, areas.log, state, now, workspace);
            detail::render(frame, areas.detail, state, now, workspace);
        }
        BulletinLayout::Tiny => {
            detail::render_tiny(frame, inner, state, now, workspace);
        }
    }
}

//! Responsive geometry for the Bulletin workspace.

use ratatui::layout::{Constraint, Layout, Rect};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BulletinLayout {
    Wide,
    Compact,
    Tiny,
}

impl BulletinLayout {
    pub(super) const fn from_rect(area: Rect) -> Self {
        if area.width >= 100 && area.height >= 20 {
            Self::Wide
        } else if area.width >= 76 && area.height >= 16 {
            Self::Compact
        } else {
            Self::Tiny
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BulletinAreas {
    pub briefing: Rect,
    pub filters: Rect,
    pub log: Rect,
    pub detail: Rect,
}

pub(super) fn wide_areas(area: Rect) -> BulletinAreas {
    let [briefing, filters, body] = Layout::vertical([
        Constraint::Length(8),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);
    let [log, detail] = Layout::horizontal([Constraint::Percentage(60), Constraint::Fill(1)])
        .spacing(2)
        .areas(body);
    BulletinAreas {
        briefing,
        filters,
        log,
        detail,
    }
}

pub(super) fn compact_areas(area: Rect) -> BulletinAreas {
    let [briefing, filters, body] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);
    let [log, detail] = Layout::horizontal([Constraint::Percentage(56), Constraint::Fill(1)])
        .spacing(1)
        .areas(body);
    BulletinAreas {
        briefing,
        filters,
        log,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulletin_layout_switches_at_workspace_boundaries() {
        assert_eq!(
            BulletinLayout::from_rect(Rect::new(0, 0, 120, 40)),
            BulletinLayout::Wide
        );
        assert_eq!(
            BulletinLayout::from_rect(Rect::new(0, 0, 90, 20)),
            BulletinLayout::Compact
        );
        assert_eq!(
            BulletinLayout::from_rect(Rect::new(0, 0, 75, 20)),
            BulletinLayout::Tiny
        );
    }

    #[test]
    fn wide_layout_reserves_space_for_the_briefing() {
        let area = Rect::new(0, 0, 120, 40);
        let areas = wide_areas(area);
        assert_eq!(areas.briefing.height, 8);
        assert_eq!(areas.filters.height, 1);
        assert!(areas.log.height > 0);
        assert!(areas.detail.height > 0);
    }

    #[test]
    fn compact_layout_keeps_log_and_detail_side_by_side() {
        let area = Rect::new(0, 0, 90, 20);
        let areas = compact_areas(area);
        assert_eq!(areas.briefing.height, 2);
        assert_eq!(areas.filters.height, 1);
        assert_eq!(areas.log.y, areas.detail.y);
        assert_eq!(areas.log.height, areas.detail.height);
        assert!(areas.log.width > areas.detail.width);
    }
}

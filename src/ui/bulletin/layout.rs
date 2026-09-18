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
    pub summary: Rect,
    pub log: Rect,
    pub detail: Rect,
}

pub(super) fn wide_areas(area: Rect) -> BulletinAreas {
    let [summary, body] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)])
        .spacing(1)
        .areas(area);
    let [log, detail] = Layout::horizontal([Constraint::Percentage(60), Constraint::Fill(1)])
        .spacing(2)
        .areas(body);
    BulletinAreas {
        summary,
        log,
        detail,
    }
}

pub(super) fn compact_areas(area: Rect) -> BulletinAreas {
    let [summary, body] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)])
        .spacing(1)
        .areas(area);
    let [log, detail] = Layout::vertical([Constraint::Percentage(55), Constraint::Fill(1)])
        .spacing(1)
        .areas(body);
    BulletinAreas {
        summary,
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
}

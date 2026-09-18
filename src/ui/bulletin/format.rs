//! Shared Bulletin presentation formatting.

use ratatui::style::Style;

use crate::{
    model::{BulletinCategory, UtcSeconds},
    ui::theme,
};

pub(super) fn category_label(category: BulletinCategory) -> &'static str {
    match category {
        BulletinCategory::Local => "LOCAL",
        BulletinCategory::Authority => "AUTHORITY",
        BulletinCategory::Construction => "CONSTRUCTION",
        BulletinCategory::Network => "NETWORK",
    }
}

pub(super) fn category_style(category: BulletinCategory) -> Style {
    match category {
        BulletinCategory::Local => theme::primary_value(),
        BulletinCategory::Authority => theme::focused_title(),
        BulletinCategory::Construction => theme::warning(),
        BulletinCategory::Network => theme::success(),
    }
}

pub(super) fn relative_time(timestamp: UtcSeconds, now: UtcSeconds) -> String {
    let seconds = now
        .unix_seconds()
        .saturating_sub(timestamp.unix_seconds())
        .max(0) as u64;
    if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

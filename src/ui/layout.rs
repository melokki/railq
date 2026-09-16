//! Shared responsive layout language for RailQ workspaces.
//!
//! Workspaces should ask for a semantic UI size instead of growing their own
//! width/height checks. The current thresholds preserve the existing Fleet
//! behaviour while giving later UI batches one place to evolve responsiveness.

use ratatui::layout::Rect;

/// Minimum workspace dimensions for the full split-view presentation.
pub const WIDE_MIN_COLUMNS: u16 = 96;
pub const WIDE_MIN_ROWS: u16 = 18;

/// Minimum workspace dimensions for the compact presentation.
pub const COMPACT_MIN_COLUMNS: u16 = 76;
pub const COMPACT_MIN_ROWS: u16 = 12;

/// Semantic responsive sizes shared by RailQ workspaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiSize {
    Tiny,
    Compact,
    Wide,
}

impl UiSize {
    /// Classifies the available workspace rectangle.
    pub const fn from_rect(area: Rect) -> Self {
        if area.width >= WIDE_MIN_COLUMNS && area.height >= WIDE_MIN_ROWS {
            Self::Wide
        } else if area.width >= COMPACT_MIN_COLUMNS && area.height >= COMPACT_MIN_ROWS {
            Self::Compact
        } else {
            Self::Tiny
        }
    }

    /// Returns whether there is room for a side-by-side picker and inspector.
    pub const fn supports_split_view(self) -> bool {
        matches!(self, Self::Wide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_workspace_sizes_at_boundaries() {
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 120, 40)), UiSize::Wide);
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 96, 18)), UiSize::Wide);
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 95, 18)), UiSize::Compact);
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 80, 24)), UiSize::Compact);
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 75, 24)), UiSize::Tiny);
        assert_eq!(UiSize::from_rect(Rect::new(0, 0, 64, 16)), UiSize::Tiny);
    }

    #[test]
    fn split_view_is_reserved_for_wide_workspaces() {
        assert!(UiSize::Wide.supports_split_view());
        assert!(!UiSize::Compact.supports_split_view());
        assert!(!UiSize::Tiny.supports_split_view());
    }
}

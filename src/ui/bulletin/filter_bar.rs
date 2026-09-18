//! Inline Bulletin view selector and category counts.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    model::{BulletinCategory, GameState},
    ui::theme,
};

use super::{BulletinFilter, BulletinWorkspace, format::category_style};

const FILTERS: [BulletinFilter; 5] = [
    BulletinFilter::All,
    BulletinFilter::Local,
    BulletinFilter::Authority,
    BulletinFilter::Construction,
    BulletinFilter::Network,
];

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    workspace: &BulletinWorkspace,
) {
    let mut spans = vec![Span::styled("VIEW  ", theme::table_header())];

    for (index, filter) in FILTERS.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", theme::secondary()));
        }
        spans.extend(filter_spans(
            filter,
            category_count(state, filter),
            filter == workspace.filter(),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme::panel()),
        area,
    );
}

fn filter_spans(filter: BulletinFilter, count: usize, selected: bool) -> Vec<Span<'static>> {
    let label = format!("{} {count}", filter.label().to_ascii_uppercase());
    if selected {
        vec![
            Span::styled("[", theme::focused_title()),
            Span::styled(label, filter_style(filter).bold()),
            Span::styled("]", theme::focused_title()),
        ]
    } else {
        vec![Span::styled(label, filter_style(filter))]
    }
}

fn filter_style(filter: BulletinFilter) -> Style {
    match filter {
        BulletinFilter::All => theme::primary_value(),
        BulletinFilter::Local => category_style(BulletinCategory::Local),
        BulletinFilter::Authority => category_style(BulletinCategory::Authority),
        BulletinFilter::Construction => category_style(BulletinCategory::Construction),
        BulletinFilter::Network => category_style(BulletinCategory::Network),
    }
}

fn category_count(state: &GameState, filter: BulletinFilter) -> usize {
    state
        .region
        .bulletin
        .iter()
        .filter(|entry| filter.matches(entry.category))
        .count()
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{BulletinCategory, BulletinEntry, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{BulletinFilter, category_count};

    #[test]
    fn category_counts_include_all_and_each_bulletin_type() {
        let mut state = create_new_game(7, "Count Rail", UtcSeconds::from_unix_seconds(1));
        state.region.bulletin = vec![
            BulletinEntry {
                occurred_at: UtcSeconds::from_unix_seconds(10),
                category: BulletinCategory::Local,
                headline: "Local".into(),
                detail: "Detail".into(),
            },
            BulletinEntry {
                occurred_at: UtcSeconds::from_unix_seconds(20),
                category: BulletinCategory::Authority,
                headline: "Authority".into(),
                detail: "Detail".into(),
            },
            BulletinEntry {
                occurred_at: UtcSeconds::from_unix_seconds(30),
                category: BulletinCategory::Authority,
                headline: "Authority again".into(),
                detail: "Detail".into(),
            },
        ];

        assert_eq!(category_count(&state, BulletinFilter::All), 3);
        assert_eq!(category_count(&state, BulletinFilter::Local), 1);
        assert_eq!(category_count(&state, BulletinFilter::Authority), 2);
        assert_eq!(category_count(&state, BulletinFilter::Construction), 0);
        assert_eq!(category_count(&state, BulletinFilter::Network), 0);
    }
}

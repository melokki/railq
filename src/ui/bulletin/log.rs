//! Bulletin development chronology rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap},
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DevelopmentGroup {
    NewSinceLastVisit,
    Today,
    Yesterday,
    Earlier,
}

impl DevelopmentGroup {
    const fn label(self) -> &'static str {
        match self {
            Self::NewSinceLastVisit => "NEW SINCE LAST VISIT",
            Self::Today => "TODAY",
            Self::Yesterday => "YESTERDAY",
            Self::Earlier => "EARLIER",
        }
    }
}

enum ChronologyRow<'a> {
    Group(DevelopmentGroup),
    Development {
        visible_index: usize,
        entry: &'a BulletinEntry,
        is_new: bool,
    },
}

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
                Line::styled(
                    "No Bulletin items in this category yet.",
                    theme::secondary(),
                ),
                Line::styled(
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

    let seen_count = workspace.visit_seen_count(state);
    let rows = chronology_rows(&entries, seen_count, now);
    let selected_visible_index = workspace.table_state_mut().selected();
    let selected_row = selected_visible_index.and_then(|selected| {
        rows.iter().position(|row| {
            matches!(
                row,
                ChronologyRow::Development { visible_index, .. } if *visible_index == selected
            )
        })
    });

    let items = rows
        .into_iter()
        .map(|row| match row {
            ChronologyRow::Group(group) => ListItem::new(Line::styled(
                group.label(),
                theme::table_header(),
            )),
            ChronologyRow::Development {
                entry,
                is_new,
                ..
            } => ListItem::new(development_line(entry, now, is_new)),
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    let mut list_state = ListState::default();
    list_state.select(selected_row);
    frame.render_stateful_widget(list, body_area, &mut list_state);
}

fn chronology_rows<'a>(
    entries: &[(usize, &'a BulletinEntry)],
    seen_count: u64,
    now: UtcSeconds,
) -> Vec<ChronologyRow<'a>> {
    let mut rows = Vec::with_capacity(entries.len().saturating_mul(2));
    let mut previous_group = None;

    for (visible_index, (bulletin_index, entry)) in entries.iter().enumerate() {
        let group = development_group(*bulletin_index, entry, seen_count, now);
        if previous_group != Some(group) {
            rows.push(ChronologyRow::Group(group));
            previous_group = Some(group);
        }
        rows.push(ChronologyRow::Development {
            visible_index,
            entry: *entry,
            is_new: group == DevelopmentGroup::NewSinceLastVisit,
        });
    }

    rows
}

fn development_group(
    bulletin_index: usize,
    entry: &BulletinEntry,
    seen_count: u64,
    now: UtcSeconds,
) -> DevelopmentGroup {
    let index = u64::try_from(bulletin_index).unwrap_or(u64::MAX);
    if index >= seen_count {
        return DevelopmentGroup::NewSinceLastVisit;
    }

    let event_day = entry.occurred_at.unix_seconds().div_euclid(86_400);
    let current_day = now.unix_seconds().div_euclid(86_400);
    if event_day >= current_day {
        DevelopmentGroup::Today
    } else if event_day == current_day.saturating_sub(1) {
        DevelopmentGroup::Yesterday
    } else {
        DevelopmentGroup::Earlier
    }
}

fn development_line(entry: &BulletinEntry, now: UtcSeconds, is_new: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<10}", relative_time(entry.occurred_at, now)),
            theme::secondary(),
        ),
        Span::styled(
            format!("{:<14}", category_label(entry.category)),
            category_style(entry.category),
        ),
        Span::styled(
            entry.headline.clone(),
            if is_new {
                theme::title()
            } else {
                theme::primary_value()
            },
        ),
    ])
}

#[cfg(test)]
mod tests {
    use crate::model::{BulletinCategory, BulletinEntry, UtcSeconds};

    use super::{DevelopmentGroup, development_group};

    fn entry(at: i64) -> BulletinEntry {
        BulletinEntry {
            occurred_at: UtcSeconds::from_unix_seconds(at),
            category: BulletinCategory::Network,
            headline: "Development".into(),
            detail: "Detail".into(),
        }
    }

    #[test]
    fn unseen_entries_take_precedence_over_date_groups() {
        let now = UtcSeconds::from_unix_seconds(5 * 86_400 + 12 * 3_600);
        let old_entry = entry(2 * 86_400);
        assert_eq!(
            development_group(4, &old_entry, 4, now),
            DevelopmentGroup::NewSinceLastVisit
        );
    }

    #[test]
    fn acknowledged_entries_are_grouped_by_utc_day() {
        let now = UtcSeconds::from_unix_seconds(5 * 86_400 + 12 * 3_600);
        assert_eq!(
            development_group(0, &entry(5 * 86_400 + 60), 3, now),
            DevelopmentGroup::Today
        );
        assert_eq!(
            development_group(1, &entry(4 * 86_400 + 60), 3, now),
            DevelopmentGroup::Yesterday
        );
        assert_eq!(
            development_group(2, &entry(3 * 86_400 + 60), 3, now),
            DevelopmentGroup::Earlier
        );
    }
}

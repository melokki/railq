//! Bulletin briefing identity and lead development presentation.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
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
};

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &BulletinWorkspace,
) {
    if area.height < 4 {
        return;
    }

    let [overview_area, lead_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);

    render_overview(frame, overview_area, state, workspace);
    render_lead_development(frame, lead_area, state, now, workspace);
}

pub(super) fn render_compact(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    workspace: &BulletinWorkspace,
) {
    render_overview(frame, area, state, workspace);
}

fn render_overview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    workspace: &BulletinWorkspace,
) {
    let [briefing_area, identity_area] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)])
            .spacing(2)
            .areas(area);

    let total = state.region.bulletin.len();
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("REGIONAL RAILWAY BRIEFING", theme::table_header()),
            Line::styled(
                development_count_label(total),
                theme::secondary(),
            ),
        ])
        .style(theme::panel()),
        briefing_area,
    );

    let new_since_visit = new_since_visit(state, workspace);
    let status = if new_since_visit == 0 {
        Line::styled("Up to date", theme::secondary())
    } else {
        Line::styled(
            format!("{new_since_visit} new since last visit"),
            theme::success(),
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(state.region.name.clone(), theme::title()),
            status,
        ])
        .alignment(Alignment::Right)
        .style(theme::panel()),
        identity_area,
    );
}

fn render_lead_development(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &BulletinWorkspace,
) {
    let Some(entry) = state.region.bulletin.last() else {
        frame.render_widget(
            Paragraph::new(vec![
                section_heading("LATEST DEVELOPMENT"),
                Line::styled(
                    "No developments recorded yet.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel()),
            area,
        );
        return;
    };

    let heading = if new_since_visit(state, workspace) > 0 {
        Line::from(vec![
            Span::styled("LATEST DEVELOPMENT", theme::table_header()),
            Span::styled("  ·  NEW", theme::success().bold()),
        ])
    } else {
        section_heading("LATEST DEVELOPMENT")
    };
    let mut lines = vec![
        heading,
        Line::from(vec![
            Span::styled(category_label(entry.category), category_style(entry.category)),
            Span::styled(
                format!("  ·  {}", relative_time(entry.occurred_at, now)),
                theme::secondary(),
            ),
        ]),
        Line::styled(entry.headline.clone(), theme::title()),
    ];

    if area.height >= 4 {
        lines.push(Line::styled(
            lead_summary(&entry.detail),
            theme::secondary(),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn lead_summary(detail: &str) -> String {
    let trimmed = detail.trim();
    let Some(sentence_end) = trimmed.find(". ") else {
        return trimmed.to_owned();
    };
    trimmed[..=sentence_end].to_owned()
}

fn new_since_visit(state: &GameState, workspace: &BulletinWorkspace) -> u64 {
    u64::try_from(state.region.bulletin.len())
        .unwrap_or(u64::MAX)
        .saturating_sub(workspace.visit_seen_count(state))
}

fn development_count_label(count: usize) -> String {
    match count {
        1 => "1 development recorded".into(),
        _ => format!("{count} developments recorded"),
    }
}

#[cfg(test)]
mod tests {
    use super::{development_count_label, lead_summary};

    #[test]
    fn development_count_uses_singular_and_plural_copy() {
        assert_eq!(development_count_label(0), "0 developments recorded");
        assert_eq!(development_count_label(1), "1 development recorded");
        assert_eq!(development_count_label(2), "2 developments recorded");
    }

    #[test]
    fn lead_summary_keeps_only_the_first_sentence_when_detail_has_more_context() {
        assert_eq!(
            lead_summary("The station is open. Access-fee relief applies for seven days."),
            "The station is open."
        );
        assert_eq!(lead_summary("Single sentence"), "Single sentence");
    }
}

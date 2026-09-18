//! Ratatui rendering for the Rail Authority workspace.

use std::fmt::Write;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    model::{GameState, InfrastructureProject, InfrastructureProjectKind, UtcSeconds},
    ui::{components::panel_block, format as ui_format, theme},
};

use super::{
    ProjectSelection,
    analytics::{AuthorityDashboardSnapshot, ProgrammeStage},
    format::{
        new_line_route_label, project_next, project_scope, relative_time,
    },
    programme::{render_development_programme, stage_label, stage_style},
    project::{preferred_project_detail_height, render_selected_project},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorityLayout {
    Wide,
    Compact,
    Tiny,
}

fn authority_layout(area: Rect) -> AuthorityLayout {
    if area.width >= 100 && area.height >= 24 {
        AuthorityLayout::Wide
    } else if area.width >= 72 && area.height >= 16 {
        AuthorityLayout::Compact
    } else {
        AuthorityLayout::Tiny
    }
}

/// Renders the public Rail Authority as a dedicated read-only workspace.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    selection.synchronize(state);

    match authority_layout(area) {
        AuthorityLayout::Wide => render_wide(frame, area, state, now, selection),
        AuthorityLayout::Compact => render_compact(frame, area, state, now, selection),
        AuthorityLayout::Tiny => render_tiny(frame, area, state, now, selection),
    }
}

fn render_wide(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let snapshot = AuthorityDashboardSnapshot::from_state(state);
    let shell = panel_block("Authority", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let [overview_area, metrics_area, body_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(shell_inner);

    render_authority_overview(frame, overview_area, state, now, snapshot);
    render_authority_metrics(frame, metrics_area, state, now, snapshot);

    const MIN_PROGRAMME_HEIGHT: u16 = 7;
    let project_detail_height = preferred_project_detail_height(state, now, selection);

    if body_area.height
        >= project_detail_height
            .saturating_add(MIN_PROGRAMME_HEIGHT)
            .saturating_add(1)
    {
        let [projects_area, project_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(project_detail_height),
        ])
        .spacing(1)
        .areas(body_area);
        render_development_programme(
            frame,
            projects_area,
            state,
            now,
            selection,
            false,
            snapshot.programme,
        );
        render_selected_project(frame, project_area, state, now, selection);
    } else {
        let [projects_area, project_area] =
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                .spacing(1)
                .areas(body_area);
        render_development_programme(
            frame,
            projects_area,
            state,
            now,
            selection,
            false,
            snapshot.programme,
        );
        render_selected_project(frame, project_area, state, now, selection);
    }
}

fn render_authority_overview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    snapshot: AuthorityDashboardSnapshot,
) {
    let [programme_area, identity_area] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)])
            .spacing(2)
            .areas(area);

    let programme_state = if snapshot.programme.pipeline() == 0 {
        "PROGRAMME QUIET"
    } else {
        "PROGRAMME ACTIVE"
    };
    let programme_style = if snapshot.programme.pipeline() == 0 {
        theme::secondary()
    } else {
        theme::success()
    };
    let mut programme_lines = vec![Line::styled(programme_state, programme_style.bold())];
    programme_lines.push(next_network_change_line(state, now, snapshot));
    render_dashboard_section(frame, programme_area, programme_lines);

    let fiscal_context = snapshot.next_fiscal_period_at.map_or_else(
        || "Next allocation schedule pending".into(),
        |timestamp| {
            format!(
                "Next allocation +{} · {}",
                ui_format::money(snapshot.public_allocation),
                relative_time(timestamp, now)
            )
        },
    );
    let identity_lines = vec![
        Line::styled(state.region.rail_authority.name.clone(), theme::title()),
        Line::styled(fiscal_context, theme::secondary()),
    ];
    render_dashboard_section(frame, identity_area, identity_lines);
}

fn render_authority_metrics(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    snapshot: AuthorityDashboardSnapshot,
) {
    let [investment_area, delivery_area, network_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    let available = snapshot
        .available_investment
        .map(ui_format::money)
        .unwrap_or_else(|| "—".into());
    render_metric_card(
        frame,
        investment_area,
        "INVESTMENT CAPACITY",
        format!("{available} available"),
        format!("Treasury {}", ui_format::money(snapshot.treasury)),
        format!(
            "Reserve {}",
            ui_format::money(snapshot.maintenance_reserve)
        ),
        format!(
            "Committed {}",
            ui_format::money(snapshot.committed_investment)
        ),
        theme::success(),
    );

    let capacity_full = snapshot.free_construction == 0 && snapshot.construction_capacity > 0;
    let capacity_context = if capacity_full {
        "CAPACITY FULL".into()
    } else {
        format!("{} slots free", snapshot.free_construction)
    };
    let next_release = snapshot.next_network_change.map_or_else(
        || "No active opening scheduled".into(),
        |change| format!("Next release {}", relative_time(change.opens_at, now)),
    );
    render_metric_card(
        frame,
        delivery_area,
        "DELIVERY CAPACITY",
        format!(
            "{} / {}",
            snapshot.reserved_construction, snapshot.construction_capacity
        ),
        "slots occupied".into(),
        capacity_context,
        next_release,
        if capacity_full {
            theme::warning()
        } else {
            theme::primary_value()
        },
    );

    let network_context = snapshot
        .next_network_change
        .and_then(|change| project_by_id(state, change.project_id))
        .map(network_change_footprint)
        .unwrap_or_else(|| "No network addition underway".into());
    render_metric_card(
        frame,
        network_area,
        "NETWORK",
        format!("{} stations", snapshot.station_count),
        format!("{} segments in service", snapshot.segment_count),
        network_context,
        project_count_label(snapshot.programme.pipeline(), "project", "projects", "in pipeline"),
        theme::primary_value(),
    );
}

fn render_metric_card(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: String,
    subtitle: String,
    context: String,
    secondary_context: String,
    value_style: Style,
) {
    let block = panel_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(value, value_style.bold()),
            Line::styled(subtitle, theme::secondary()),
            Line::styled(context, theme::secondary()),
            Line::styled(secondary_context, theme::hint()),
        ])
        .style(theme::panel()),
        inner,
    );
}

fn project_count_label(count: usize, singular: &str, plural: &str, suffix: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{count} {noun} {suffix}")
}

fn next_network_change_line(
    state: &GameState,
    now: UtcSeconds,
    snapshot: AuthorityDashboardSnapshot,
) -> Line<'static> {
    let Some(change) = snapshot.next_network_change else {
        return Line::from(vec![
            Span::styled("NEXT NETWORK CHANGE  ", theme::table_header()),
            Span::styled("No opening currently scheduled", theme::secondary()),
        ]);
    };
    let Some(project) = project_by_id(state, change.project_id) else {
        return Line::from(vec![
            Span::styled("NEXT NETWORK CHANGE  ", theme::table_header()),
            Span::styled("Opening details unavailable", theme::secondary()),
        ]);
    };

    Line::from(vec![
        Span::styled("NEXT NETWORK CHANGE  ", theme::table_header()),
        Span::styled(network_change_label(state, project), theme::primary_value().bold()),
        Span::styled(
            format!(" · opens {}", relative_time(change.opens_at, now)),
            theme::secondary(),
        ),
    ])
}

fn compact_network_change_line(
    state: &GameState,
    now: UtcSeconds,
    snapshot: AuthorityDashboardSnapshot,
) -> Line<'static> {
    let Some(change) = snapshot.next_network_change else {
        return Line::from(vec![
            Span::styled("Next change  ", theme::table_header()),
            Span::styled("No opening scheduled", theme::secondary()),
        ]);
    };
    let Some(project) = project_by_id(state, change.project_id) else {
        return Line::from(vec![
            Span::styled("Next change  ", theme::table_header()),
            Span::styled("Opening details unavailable", theme::secondary()),
        ]);
    };

    Line::from(vec![
        Span::styled("Next change  ", theme::table_header()),
        Span::styled(network_change_label(state, project), theme::primary_value().bold()),
        Span::styled(
            format!(" · {}", relative_time(change.opens_at, now)),
            theme::secondary(),
        ),
    ])
}

fn project_by_id(
    state: &GameState,
    project_id: crate::model::InfrastructureProjectId,
) -> Option<&InfrastructureProject> {
    state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .find(|project| project.id == project_id)
}

fn network_change_label(state: &GameState, project: &InfrastructureProject) -> String {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => new_line_route_label(state, planned_stations, planned_lines)
            .unwrap_or_else(|| project_scope(state, project)),
        _ => project_scope(state, project),
    }
}

fn network_change_footprint(project: &InfrastructureProject) -> String {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            let metres = planned_lines.iter().fold(0_u64, |total, line| {
                total.saturating_add(line.distance.metres())
            });
            format!(
                "+{} {} · +{} underway",
                planned_stations.len(),
                if planned_stations.len() == 1 {
                    "station"
                } else {
                    "stations"
                },
                ui_format::distance(metres)
            )
        }
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
        | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
        | InfrastructureProjectKind::Electrification { rail_line_ids }
        | InfrastructureProjectKind::Renewal { rail_line_ids } => {
            format!("{} segment(s) being upgraded", rail_line_ids.len())
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            format!("{} station(s) being upgraded", rail_station_ids.len())
        }
    }
}

fn render_dashboard_section(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_compact(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let snapshot = AuthorityDashboardSnapshot::from_state(state);
    let shell = panel_block("Authority", true);
    let inner = shell.inner(area);
    frame.render_widget(shell, area);

    if inner.height >= 20 {
        let [summary_area, projects_area, inspector_area] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .areas(inner);
        render_compact_summary(frame, summary_area, state, now, snapshot);
        render_development_programme(
            frame,
            projects_area,
            state,
            now,
            selection,
            true,
            snapshot.programme,
        );
        render_selected_project(frame, inspector_area, state, now, selection);
    } else {
        let [summary_area, projects_area] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .areas(inner);
        render_compact_summary(frame, summary_area, state, now, snapshot);
        render_development_programme(
            frame,
            projects_area,
            state,
            now,
            selection,
            true,
            snapshot.programme,
        );
    }
}

fn render_compact_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    snapshot: AuthorityDashboardSnapshot,
) {
    let available = snapshot
        .available_investment
        .map(ui_format::money)
        .unwrap_or_else(|| "—".into());
    let next_allocation = snapshot.next_fiscal_period_at.map_or_else(
        || "allocation pending".into(),
        |timestamp| {
            format!(
                "+{} · {}",
                ui_format::money(snapshot.public_allocation),
                relative_time(timestamp, now)
            )
        },
    );

    let mut lines = vec![
        Line::from(vec![
            Span::styled(state.region.rail_authority.name.clone(), theme::title()),
            Span::styled(format!(" · {}", state.region.name), theme::secondary()),
        ]),
        Line::from(vec![
            Span::styled("Available  ", theme::secondary()),
            Span::styled(available, theme::success().bold()),
            Span::styled("   Treasury  ", theme::secondary()),
            Span::styled(ui_format::money(snapshot.treasury), theme::primary_value()),
        ]),
        Line::from(vec![
            Span::styled("Delivery  ", theme::secondary()),
            Span::styled(
                format!(
                    "{}/{} slots",
                    snapshot.reserved_construction, snapshot.construction_capacity
                ),
                if snapshot.free_construction == 0 && snapshot.construction_capacity > 0 {
                    theme::warning()
                } else {
                    theme::primary_value()
                },
            ),
            Span::styled("   Next allocation  ", theme::secondary()),
            Span::styled(next_allocation, theme::primary_value()),
        ]),
    ];
    lines.push(compact_network_change_line(state, now, snapshot));
    render_dashboard_section(frame, area, lines);
}

fn render_tiny(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let snapshot = AuthorityDashboardSnapshot::from_state(state);
    selection.set_page_size(usize::MAX);
    let shell = panel_block("Authority", true);
    let inner = shell.inner(area);
    frame.render_widget(shell, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let available = snapshot
        .available_investment
        .map(ui_format::money)
        .unwrap_or_else(|| "—".into());
    let mut lines = vec![
        Line::styled(state.region.rail_authority.name.clone(), theme::title()),
        Line::from(vec![
            Span::styled("Available  ", theme::secondary()),
            Span::styled(available, theme::success()),
            Span::styled(" · ", theme::secondary()),
            Span::styled(
                format!(
                    "{} projects",
                    state.region.rail_authority.infrastructure_projects.len()
                ),
                theme::primary_value(),
            ),
        ]),
        compact_network_change_line(state, now, snapshot),
    ];

    if let Some((index, project)) = selection.selected_project(state) {
        lines.push(Line::from(vec![
            Span::styled(format!("PROJECT {:02}  ", index + 1), theme::table_header()),
            Span::styled(project_scope(state, project), theme::primary_value()),
        ]));
        let stage = ProgrammeStage::from_status(project.status);
        lines.push(Line::from(vec![
            Span::styled(stage_label(stage), stage_style(stage)),
            Span::styled(" · ", theme::secondary()),
            Span::styled(project_next(state, project, now), theme::secondary()),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

/// Compact textual fallback used by very small terminals and tests.
pub fn render_text(state: &GameState, now: UtcSeconds) -> String {
    let authority = &state.region.rail_authority;
    let finances = &authority.finances;
    let available = finances
        .uncommitted_investment()
        .map(ui_format::money)
        .unwrap_or_else(|_| "—".into());
    let mut output = String::new();
    writeln!(output, "{}", authority.name).expect("writing to String cannot fail");
    writeln!(output, "Treasury: {}", ui_format::money(finances.treasury))
        .expect("writing to String cannot fail");
    writeln!(output, "Available investment: {available}").expect("writing to String cannot fail");
    writeln!(
        output,
        "Maintenance reserve: {}",
        ui_format::money(finances.maintenance_reserve)
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Committed projects: {}",
        ui_format::money(finances.committed_investment)
    )
    .expect("writing to String cannot fail");
    if let Some(next) = finances.next_fiscal_period_at {
        writeln!(
            output,
            "Next fiscal period: +{} · {}",
            ui_format::money(finances.regional_public_allocation),
            relative_time(next, now)
        )
        .expect("writing to String cannot fail");
    }
    writeln!(
        output,
        "Access-fee revenue: {}",
        ui_format::money(finances.infrastructure_access_fee_revenue)
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Construction slots: {}/{} occupied",
        authority.reserved_construction_count(),
        authority.construction_capacity
    )
    .expect("writing to String cannot fail");
    writeln!(output, "Projects:").expect("writing to String cannot fail");
    for (index, project) in authority.infrastructure_projects.iter().enumerate() {
        writeln!(
            output,
            "{:02}  {:<14}  {}  {}",
            index + 1,
            stage_label(ProgrammeStage::from_status(project.status)),
            project_scope(state, project),
            project_next(state, project, now)
        )
        .expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::{AuthorityLayout, authority_layout};

    #[test]
    fn authority_dashboard_uses_stable_responsive_breakpoints() {
        assert_eq!(
            authority_layout(Rect::new(0, 0, 120, 34)),
            AuthorityLayout::Wide
        );
        assert_eq!(
            authority_layout(Rect::new(0, 0, 96, 24)),
            AuthorityLayout::Compact
        );
        assert_eq!(
            authority_layout(Rect::new(0, 0, 72, 16)),
            AuthorityLayout::Compact
        );
        assert_eq!(
            authority_layout(Rect::new(0, 0, 71, 20)),
            AuthorityLayout::Tiny
        );
        assert_eq!(
            authority_layout(Rect::new(0, 0, 120, 15)),
            AuthorityLayout::Tiny
        );
    }
}

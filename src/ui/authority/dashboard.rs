//! Ratatui rendering for the Rail Authority workspace.

use std::fmt::Write;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
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
    analytics::AuthorityDashboardSnapshot,
    format::{
        new_line_route_label, project_next, project_scope, project_status, relative_time,
    },
    programme::{render_programme_pipeline, render_projects},
    project::render_selected_project,
};

/// Renders the public Rail Authority as a dedicated read-only workspace.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    selection.synchronize(state);

    if area.width >= 100 && area.height >= 18 {
        render_wide(frame, area, state, now, selection);
    } else if area.height >= 14 {
        render_compact(frame, area, state, now, selection);
    } else {
        render_tiny(frame, area, state, now);
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

    let [overview_area, metrics_area, pipeline_area, body_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(shell_inner);

    render_authority_overview(frame, overview_area, state, now, snapshot);
    render_authority_metrics(frame, metrics_area, state, now, snapshot);
    render_programme_pipeline(frame, pipeline_area, snapshot.programme);

    let visible_rows = u16::try_from(
        state
            .region
            .rail_authority
            .infrastructure_projects
            .len(),
    )
    .unwrap_or(7)
    .min(7);
    let projects_height = visible_rows.saturating_add(3).clamp(6, 10);

    if body_area.height >= projects_height.saturating_add(9) {
        let [projects_area, project_area] = Layout::vertical([
            Constraint::Length(projects_height),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .areas(body_area);
        render_projects(frame, projects_area, state, now, selection, false);
        render_selected_project(frame, project_area, state, now, selection);
    } else {
        let [projects_area, project_area] =
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                .spacing(1)
                .areas(body_area);
        render_projects(frame, projects_area, state, now, selection, false);
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
    let mut programme_lines = vec![Line::from(vec![
        Span::styled(programme_state, programme_style.bold()),
        Span::styled(
            format!(
                " · {} building · {} funding · {} open",
                snapshot.active_construction, snapshot.programme.funding, snapshot.programme.open
            ),
            theme::secondary(),
        ),
    ])];
    programme_lines.push(next_network_change_line(state, now, snapshot));
    render_dashboard_section(frame, programme_area, programme_lines);

    let fiscal_context = snapshot.next_fiscal_period_at.map_or_else(
        || "allocation schedule pending".into(),
        |timestamp| format!("next allocation {}", relative_time(timestamp, now)),
    );
    let identity_lines = vec![
        Line::styled(state.region.rail_authority.name.clone(), theme::title()),
        Line::from(vec![
            Span::styled(state.region.name.clone(), theme::secondary()),
            Span::styled(format!(" · {fiscal_context}"), theme::hint()),
        ]),
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
        "construction slots reserved".into(),
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
        format!("{} projects in pipeline", snapshot.programme.pipeline()),
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
        .alignment(Alignment::Center)
        .style(theme::panel()),
        inner,
    );
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
    let [summary_area, projects_area, inspector_area] = Layout::vertical([
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Fill(1),
    ])
    .areas(area);
    render_finances(frame, summary_area, state, now);
    render_projects(frame, projects_area, state, now, selection, true);
    render_selected_project(frame, inspector_area, state, now, selection);
}

fn render_tiny(frame: &mut Frame, area: Rect, state: &GameState, now: UtcSeconds) {
    frame.render_widget(
        Paragraph::new(render_text(state, now))
            .block(panel_block("Rail Authority", false))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_finances(frame: &mut Frame, area: Rect, state: &GameState, now: UtcSeconds) {
    let authority = &state.region.rail_authority;
    let snapshot = AuthorityDashboardSnapshot::from_state(state);
    let available = snapshot
        .available_investment
        .map(ui_format::money)
        .unwrap_or_else(|| "—".into());
    let next_allocation = snapshot.next_fiscal_period_at.map_or_else(
        || "Scheduling pending".into(),
        |timestamp| {
            format!(
                "+{} · {}",
                ui_format::money(snapshot.public_allocation),
                relative_time(timestamp, now)
            )
        },
    );

    let lines = vec![
        Line::from(vec![
            Span::styled("Authority  ", theme::secondary()),
            Span::styled(authority.name.clone(), theme::title()),
        ]),
        Line::from(vec![
            Span::styled("Available to invest  ", theme::secondary()),
            Span::styled(available, theme::success().bold()),
        ]),
        Line::from(vec![
            Span::styled("Treasury  ", theme::secondary()),
            Span::styled(ui_format::money(snapshot.treasury), theme::primary_value()),
        ]),
        Line::from(vec![
            Span::styled("Allocated  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} reserve · {} committed",
                    ui_format::money(snapshot.maintenance_reserve),
                    ui_format::money(snapshot.committed_investment)
                ),
                theme::primary_value(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Next public allocation  ", theme::secondary()),
            Span::styled(next_allocation, theme::primary_value()),
        ]),
        Line::from(vec![
            Span::styled("Access-fee revenue  ", theme::secondary()),
            Span::styled(
                ui_format::money(snapshot.access_fee_revenue),
                theme::primary_value(),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Investment Capacity", false))
            .style(theme::panel()),
        area,
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
        "Construction slots: {}/{} reserved",
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
            project_status(project.status),
            project_scope(state, project),
            project_next(state, project, now)
        )
        .expect("writing to String cannot fail");
    }
    output
}

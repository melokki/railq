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
    model::{
        GameState, InfrastructureProject, InfrastructureProjectKind, InfrastructureProjectStatus,
        Money, PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS,
        PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS, UtcSeconds,
    },
    sim::authority::{
        AUTHORITY_APPROVAL_SCORE_THRESHOLD, AUTHORITY_REJECTION_SCORE_THRESHOLD,
        local_rail_success_basis_points, project_connection_station_id,
        project_review_score_breakdown,
    },
    ui::{components::panel_block, format as ui_format, theme},
};

use super::{
    ProjectSelection,
    analytics::AuthorityDashboardSnapshot,
    format::{
        access_discount_label, construction_remaining_duration, deferred_next,
        difficulty_label, duration_line, electrification_label, format_project_timestamp,
        maturity_label, maturity_percent, money_line, new_line_route_label,
        progress_line, project_next, project_scope, project_status, relative_time, schedule_line,
        settlement_name, short_uuid, status_style, timestamp_line, value_line,
    },
    programme::{render_programme_pipeline, render_projects},
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

    let [projects_area, inspector_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .areas(body_area);
    render_projects(frame, projects_area, state, now, selection, false);
    render_project_inspector(frame, inspector_area, state, now, selection);
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

    let identity_lines = vec![
        Line::styled(state.region.rail_authority.name.clone(), theme::title()),
        Line::from(vec![
            Span::styled(state.region.name.clone(), theme::secondary()),
            Span::styled(" · public infrastructure programme", theme::hint()),
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
    let next_allocation = snapshot.next_fiscal_period_at.map_or_else(
        || "Next allocation pending".into(),
        |timestamp| {
            format!(
                "+{} {}",
                ui_format::money(snapshot.public_allocation),
                relative_time(timestamp, now)
            )
        },
    );
    render_metric_card(
        frame,
        investment_area,
        "INVESTMENT CAPACITY",
        available,
        "available to invest".into(),
        format!("Treasury {}", ui_format::money(snapshot.treasury)),
        next_allocation,
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
    render_project_inspector(frame, inspector_area, state, now, selection);
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

    let lines = vec![
        Line::from(vec![
            Span::styled("Authority  ", theme::secondary()),
            Span::styled(authority.name.clone(), theme::title()),
        ]),
        money_line("Treasury", snapshot.treasury),
        Line::from(vec![
            Span::styled("Available investment  ", theme::secondary()),
            Span::styled(available, theme::success()),
        ]),
        money_line("Maintenance reserve", snapshot.maintenance_reserve),
        money_line("Committed projects", snapshot.committed_investment),
        money_line("Daily public allocation", snapshot.public_allocation),
        money_line("Access-fee revenue", snapshot.access_fee_revenue),
        snapshot
            .next_fiscal_period_at
            .map(|timestamp| schedule_line("Next fiscal period", timestamp, now))
            .unwrap_or_else(|| value_line("Next fiscal period", "Scheduling pending")),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Infrastructure Finances", false))
            .style(theme::panel()),
        area,
    );
}

fn render_project_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let Some((index, project)) = selection.selected_project(state) else {
        frame.render_widget(
            Paragraph::new("No project selected.")
                .block(panel_block("Selected Project", false))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let gap = project
        .funding
        .funding_gap()
        .map(ui_format::money)
        .unwrap_or_else(|_| "—".into());
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("Project {:02}  ", index + 1), theme::title()),
            Span::styled(short_uuid(project.id), theme::secondary()),
        ]),
        Line::from(vec![
            Span::styled("Scope  ", theme::secondary()),
            Span::styled(project_scope(state, project), theme::primary_value()),
        ]),
        Line::from(vec![
            Span::styled("Status  ", theme::secondary()),
            Span::styled(project_status(project.status), status_style(project.status)),
        ]),
        Line::from(vec![
            Span::styled("Next  ", theme::secondary()),
            Span::styled(project_next(state, project, now), theme::primary_value()),
        ]),
    ];

    append_project_development_context(&mut lines, state, project, now);
    lines.push(Line::from(""));
    lines.extend([
        money_line("Estimated cost", project.funding.estimated_cost),
        money_line("Authority committed", project.funding.authority_committed),
        money_line(
            "Operator contribution",
            project.funding.operator_contributed,
        ),
        Line::from(vec![
            Span::styled("Funding gap  ", theme::secondary()),
            Span::styled(gap, theme::primary_value()),
        ]),
    ]);
    if let Some(discount) = project.funding.access_fee_discount {
        let active = discount.is_active_at(now);
        lines.push(Line::from(vec![
            Span::styled("Access discount  ", theme::secondary()),
            Span::styled(
                access_discount_label(discount, now),
                if active {
                    theme::success()
                } else {
                    theme::secondary()
                },
            ),
        ]));
    } else if project.funding.operator_contributed > Money::ZERO {
        let percent = u32::from(PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS) / 100;
        lines.push(Line::from(vec![
            Span::styled("Projected access discount  ", theme::secondary()),
            Span::styled(
                format!(
                    "{percent}% for {} fiscal days",
                    PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS
                ),
                theme::success(),
            ),
        ]));
    }

    append_project_scope_details(&mut lines, state, project);
    lines.push(Line::from(""));
    append_timeline(&mut lines, state, project, now);

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Selected Project", false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn append_project_development_context(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
    now: UtcSeconds,
) {
    let InfrastructureProjectKind::NewLine {
        planned_stations, ..
    } = &project.kind
    else {
        return;
    };
    let Some(planned_station) = planned_stations.first() else {
        return;
    };

    lines.push(Line::from(""));
    lines.push(Line::styled("DEVELOPMENT CASE", theme::table_header()));
    lines.push(Line::from(vec![
        Span::styled("Requested by  ", theme::secondary()),
        Span::styled(
            format!(
                "{} Council",
                settlement_name(state, planned_station.settlement_id)
            ),
            theme::primary_value(),
        ),
    ]));

    if let Some(connection_station_id) = project_connection_station_id(project) {
        let maturity = local_rail_success_basis_points(
            &state.origin_destination_demand,
            connection_station_id,
        );
        lines.push(Line::from(vec![
            Span::styled("Nearby rail adoption  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} · {}",
                    maturity_percent(maturity),
                    maturity_label(maturity)
                ),
                theme::primary_value(),
            ),
        ]));
    }

    if matches!(
        project.status,
        InfrastructureProjectStatus::Requested
            | InfrastructureProjectStatus::UnderReview
            | InfrastructureProjectStatus::Proposed
            | InfrastructureProjectStatus::Deferred
    ) {
        if let Some(score) =
            project_review_score_breakdown(&state.region, project, state.world_seed)
        {
            lines.push(Line::from(vec![
                Span::styled("Current case  ", theme::secondary()),
                Span::styled(
                    format!("{} / {}", score.total, AUTHORITY_APPROVAL_SCORE_THRESHOLD),
                    if score.total >= AUTHORITY_APPROVAL_SCORE_THRESHOLD {
                        theme::success()
                    } else {
                        theme::warning()
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Public value  ", theme::secondary()),
                Span::styled(
                    format!(
                        "+{} population · +{} demand · +{} network",
                        score.population, score.latent_demand, score.network_usefulness
                    ),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Regional / cost  ", theme::secondary()),
                Span::styled(
                    format!(
                        "+{} development · -{} construction",
                        score.regional_development, score.construction_cost_penalty
                    ),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Decision bands  ", theme::secondary()),
                Span::styled(
                    format!(
                        "approve ≥{} · defer {}–{} · reject <{}",
                        AUTHORITY_APPROVAL_SCORE_THRESHOLD,
                        AUTHORITY_REJECTION_SCORE_THRESHOLD,
                        AUTHORITY_APPROVAL_SCORE_THRESHOLD - 1,
                        AUTHORITY_REJECTION_SCORE_THRESHOLD,
                    ),
                    theme::secondary(),
                ),
            ]));
        }
    }

    match project.status {
        InfrastructureProjectStatus::Deferred => {
            lines.push(Line::from(vec![
                Span::styled("Reconsideration  ", theme::secondary()),
                Span::styled(deferred_next(state, project, now), theme::primary_value()),
            ]));
        }
        InfrastructureProjectStatus::Rejected => {
            lines.push(value_line("Reconsideration", "Not automatic"));
        }
        _ => {}
    }

    if project.timeline.reconsideration_count > 0 {
        lines.push(value_line(
            "Reconsiderations",
            &project.timeline.reconsideration_count.to_string(),
        ));
    }
}

fn append_project_scope_details(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
) {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            let total_metres = planned_lines
                .iter()
                .map(|line| line.distance.metres())
                .sum::<u64>();
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "PLANNED INFRASTRUCTURE",
                theme::table_header(),
            ));
            if let Some(route) = new_line_route_label(state, planned_stations, planned_lines) {
                lines.push(Line::from(vec![
                    Span::styled("Route  ", theme::secondary()),
                    Span::styled(route, theme::primary_value()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("New stations  ", theme::secondary()),
                Span::styled(
                    planned_stations
                        .iter()
                        .map(|station| settlement_name(state, station.settlement_id))
                        .collect::<Vec<_>>()
                        .join(", "),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("New track  ", theme::secondary()),
                Span::styled(ui_format::distance(total_metres), theme::primary_value()),
            ]));
            if let Some(line) = planned_lines.first() {
                lines.push(Line::from(vec![
                    Span::styled("Initial capability  ", theme::secondary()),
                    Span::styled(
                        format!(
                            "{} km/h · {} track{} · {} · {} difficulty",
                            line.speed_limit.kilometres_per_hour(),
                            line.track_count.tracks(),
                            if line.track_count.tracks() == 1 {
                                ""
                            } else {
                                "s"
                            },
                            electrification_label(line.electrification),
                            difficulty_label(line.construction_difficulty)
                        ),
                        theme::primary_value(),
                    ),
                ]));
            }
        }
        InfrastructureProjectKind::SpeedUpgrade {
            rail_line_ids,
            target_speed_limit,
        } => lines.push(Line::from(format!(
            "{} segment(s) → {} km/h",
            rail_line_ids.len(),
            target_speed_limit.kilometres_per_hour()
        ))),
        InfrastructureProjectKind::DoubleTracking {
            rail_line_ids,
            target_track_count,
        } => lines.push(Line::from(format!(
            "{} segment(s) → {} tracks",
            rail_line_ids.len(),
            target_track_count.tracks()
        ))),
        InfrastructureProjectKind::Electrification { rail_line_ids } => lines.push(Line::from(
            format!("Electrify {} segment(s)", rail_line_ids.len()),
        )),
        InfrastructureProjectKind::Renewal { rail_line_ids } => lines.push(Line::from(format!(
            "Renew {} segment(s)",
            rail_line_ids.len()
        ))),
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => lines.push(Line::from(
            format!("Upgrade {} station(s)", rail_station_ids.len()),
        )),
    }
}

fn append_timeline(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
    now: UtcSeconds,
) {
    lines.push(Line::styled("PROJECT TIMELINE", theme::table_header()));
    let request_label = if matches!(&project.kind, InfrastructureProjectKind::NewLine { .. }) {
        "Council request"
    } else {
        "Requested"
    };
    lines.push(timestamp_line(
        request_label,
        project.timeline.requested_at,
        now,
    ));
    if let Some(value) = project.timeline.approved_at {
        lines.push(timestamp_line("Approved", value, now));
    }
    if let Some(value) = project.timeline.funding_completed_at {
        lines.push(timestamp_line("Funded", value, now));
    }

    lines.push(Line::from(""));
    match project.status {
        InfrastructureProjectStatus::Requested => {
            lines.push(Line::styled("REQUESTED", theme::table_header()));
            lines.push(value_line("Next", &project_next(state, project, now)));
        }
        InfrastructureProjectStatus::UnderReview => {
            lines.push(Line::styled("REVIEW", theme::table_header()));
            if let Some(value) = project.timeline.review_started_at {
                lines.push(timestamp_line("Started", value, now));
            }
            lines.push(value_line("Next", &project_next(state, project, now)));
        }
        InfrastructureProjectStatus::Proposed => {
            lines.push(Line::styled("PROPOSAL", theme::table_header()));
            if let Some(value) = project.timeline.proposed_at {
                lines.push(timestamp_line("Proposed", value, now));
            }
            lines.push(value_line("Next", &project_next(state, project, now)));
        }
        InfrastructureProjectStatus::Approved => {
            lines.push(Line::styled("APPROVED", theme::table_header()));
            lines.push(value_line("Next", "Awaiting funding slot"));
        }
        InfrastructureProjectStatus::Deferred => {
            lines.push(Line::styled("DEFERRED", theme::table_header()));
            if let Some(value) = project.timeline.deferred_at {
                lines.push(timestamp_line("Deferred", value, now));
            }
            lines.push(value_line("Next", &deferred_next(state, project, now)));
        }
        InfrastructureProjectStatus::Rejected => {
            lines.push(Line::styled("REJECTED", theme::table_header()));
            lines.push(value_line("Next", "No automatic reconsideration"));
        }
        InfrastructureProjectStatus::Funding => {
            lines.push(Line::styled("FUNDING", theme::table_header()));
            let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
            let committed = project
                .funding
                .total_funded()
                .map(|money| i128::from(money.cents()).max(0))
                .unwrap_or(0);
            let percent = if estimated == 0 {
                0
            } else {
                committed
                    .saturating_mul(100)
                    .saturating_div(estimated)
                    .min(100) as u8
            };
            lines.push(progress_line("Progress", percent));
            if let Ok(gap) = project.funding.funding_gap() {
                lines.push(money_line("Remaining", gap));
            }
        }
        InfrastructureProjectStatus::Scheduled => {
            lines.push(Line::styled("CONSTRUCTION QUEUE", theme::table_header()));
            if let Some(value) = project.timeline.scheduled_start_at {
                lines.push(schedule_line("Starts", value, now));
            } else {
                lines.push(value_line("Starts", "Awaiting construction slot"));
            }
        }
        InfrastructureProjectStatus::Construction => {
            lines.push(Line::styled("CONSTRUCTION", theme::table_header()));
            if let Some(started) = project.timeline.construction_started_at {
                lines.push(timestamp_line("Started", started, now));
                if let Some(completion) = project.timeline.planned_completion_at {
                    let duration = completion
                        .unix_seconds()
                        .saturating_sub(started.unix_seconds())
                        .max(0) as u64;
                    let elapsed = now
                        .unix_seconds()
                        .saturating_sub(started.unix_seconds())
                        .max(0) as u64;
                    let remaining = completion
                        .unix_seconds()
                        .saturating_sub(now.unix_seconds())
                        .max(0) as u64;
                    let progress = if duration == 0 {
                        100
                    } else {
                        elapsed
                            .min(duration)
                            .saturating_mul(100)
                            .saturating_div(duration) as u8
                    };

                    lines.push(duration_line("Duration", duration));
                    lines.push(Line::from(vec![
                        Span::styled("Opens  ", theme::secondary()),
                        Span::styled(
                            format_project_timestamp(completion, now),
                            theme::primary_value(),
                        ),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Remaining  ", theme::secondary()),
                        Span::styled(
                            construction_remaining_duration(remaining),
                            theme::primary_value(),
                        ),
                    ]));
                    lines.push(progress_line("Progress", progress));
                }
            }
        }
        InfrastructureProjectStatus::Open => {
            lines.push(Line::styled("OPEN", theme::table_header()));
            if let Some(value) = project.timeline.completed_at {
                lines.push(Line::from(vec![
                    Span::styled("Opened  ", theme::secondary()),
                    Span::styled(format_project_timestamp(value, now), theme::primary_value()),
                ]));
            }
        }
        InfrastructureProjectStatus::Cancelled => {
            lines.push(Line::styled("CANCELLED", theme::table_header()));
            if let Some(value) = project.timeline.cancelled_at {
                lines.push(timestamp_line("Cancelled", value, now));
            }
        }
    }
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
    if let Some(next) = finances.next_fiscal_period_at {
        writeln!(output, "Next fiscal period: {}", relative_time(next, now))
            .expect("writing to String cannot fail");
    }
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

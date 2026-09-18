//! Ratatui rendering for the Rail Authority workspace.

use std::fmt::Write;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table, Wrap},
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
        funding_percent, maturity_label, maturity_percent, money_line, new_line_route_label,
        progress_line, project_next, project_scope, project_status, relative_time, schedule_line,
        settlement_name, short_uuid, status_count_line, status_style, timestamp_line, value_line,
    },
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
    let [summary_area, body_area] =
        Layout::vertical([Constraint::Length(10), Constraint::Fill(1)]).areas(area);
    let [finance_area, programme_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .areas(summary_area);
    render_finances(frame, finance_area, state, now);
    render_programme(frame, programme_area, state);

    let [projects_area, inspector_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .areas(body_area);
    render_projects(frame, projects_area, state, now, selection, false);
    render_project_inspector(frame, inspector_area, state, now, selection);
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

fn render_programme(frame: &mut Frame, area: Rect, state: &GameState) {
    let projects = &state.region.rail_authority.infrastructure_projects;
    let snapshot = AuthorityDashboardSnapshot::from_state(state);
    let active = snapshot.active_construction;
    let reserved = snapshot.reserved_construction;
    let open = snapshot.programme.open;
    let pipeline = snapshot.programme.pipeline();

    let lines = vec![
        Line::from(vec![
            Span::styled("Network  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} stations · {} segments",
                    snapshot.station_count,
                    snapshot.segment_count
                ),
                theme::primary_value(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Projects  ", theme::secondary()),
            Span::styled(
                format!("{} pipeline · {open} open", pipeline),
                theme::primary_value(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Construction slots  ", theme::secondary()),
            Span::styled(
                format!(
                    "{reserved}/{} reserved · {active} active · {} free",
                    snapshot.construction_capacity,
                    snapshot.free_construction
                ),
                if snapshot.free_construction == 0 {
                    theme::warning()
                } else {
                    theme::primary_value()
                },
            ),
        ]),
        status_count_line(projects, InfrastructureProjectStatus::Funding, "Funding"),
        status_count_line(
            projects,
            InfrastructureProjectStatus::Scheduled,
            "Scheduled",
        ),
        status_count_line(
            projects,
            InfrastructureProjectStatus::Construction,
            "Construction",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Development Programme", false))
            .style(theme::panel()),
        area,
    );
}

fn render_projects(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
    compact: bool,
) {
    let projects = &state.region.rail_authority.infrastructure_projects;
    if projects.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("No infrastructure projects yet.", theme::secondary()),
                Line::styled(
                    "Local councils will request connections as nearby rail adoption grows.",
                    theme::secondary(),
                ),
            ])
            .block(panel_block("Infrastructure Projects", false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let inner_height = area.height.saturating_sub(3);
    selection.set_page_size(usize::from(inner_height).max(1));
    selection.synchronize(state);

    let rows = projects
        .iter()
        .enumerate()
        .map(|(index, project)| {
            let scope = project_scope(state, project);
            let status = project_status(project.status);
            let next = project_next(state, project, now);
            if compact {
                Row::new(vec![
                    Cell::from(format!("{:02}", index + 1)),
                    Cell::from(scope),
                    Cell::from(status),
                ])
            } else {
                Row::new(vec![
                    Cell::from(format!("{:02}", index + 1)),
                    Cell::from(scope),
                    Cell::from(status).style(status_style(project.status)),
                    Cell::from(
                        Text::from(ui_format::money(project.funding.estimated_cost)).right_aligned(),
                    ),
                    Cell::from(Text::from(funding_percent(project)).right_aligned()),
                    Cell::from(next),
                ])
            }
        })
        .collect::<Vec<_>>();

    let (header, widths) = if compact {
        (
            Row::new(["#", "Project", "Status"]).style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(vec![
                Cell::from("#"),
                Cell::from("Project"),
                Cell::from("Status"),
                Cell::from(Text::from("Cost").right_aligned()),
                Cell::from(Text::from("Funded").right_aligned()),
                Cell::from("Next milestone"),
            ])
            .style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Fill(2),
                Constraint::Length(14),
                Constraint::Length(13),
                Constraint::Length(8),
                Constraint::Fill(1),
            ],
        )
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(panel_block("Infrastructure Projects", false))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
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

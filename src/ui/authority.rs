//! Rail Authority infrastructure programme presentation.
//!
//! This workspace is read-only. It exposes the public infrastructure budget,
//! construction capacity, and persisted project pipeline without allowing the
//! Player Company to control Authority decisions directly.

use std::fmt::Write;

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{
        ConstructionDifficulty, Electrification, GameState, InfrastructureProject,
        InfrastructureProjectId, InfrastructureProjectKind, InfrastructureProjectStatus, Money,
        UtcSeconds,
    },
    ui::{format, theme},
};

/// Persistent read-only project focus for the Authority workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectSelection {
    selected_project_id: Option<InfrastructureProjectId>,
    table_state: TableState,
    page_size: usize,
}

impl ProjectSelection {
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let projects = &state.region.rail_authority.infrastructure_projects;
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k' | 'K') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j' | 'J') => selected
                .saturating_add(1)
                .min(projects.len().saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(projects.len().saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    fn synchronize(&mut self, state: &GameState) {
        let projects = &state.region.rail_authority.infrastructure_projects;
        let previous_index = self.table_state.selected();
        let selected = self
            .selected_project_id
            .and_then(|project_id| projects.iter().position(|project| project.id == project_id))
            .or_else(|| previous_index.map(|index| index.min(projects.len().saturating_sub(1))))
            .or_else(|| {
                projects.iter().position(|project| {
                    !matches!(
                        project.status,
                        InfrastructureProjectStatus::Open
                            | InfrastructureProjectStatus::Cancelled
                    )
                })
            })
            .or_else(|| (!projects.is_empty()).then_some(projects.len().saturating_sub(1)));
        if let Some(index) = selected {
            self.selected_project_id = Some(projects[index].id);
        } else {
            self.selected_project_id = None;
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(project) = state.region.rail_authority.infrastructure_projects.get(index) else {
            return;
        };
        self.selected_project_id = Some(project.id);
        self.table_state.select(Some(index));
    }

    fn selected_project<'a>(
        &mut self,
        state: &'a GameState,
    ) -> Option<(usize, &'a InfrastructureProject)> {
        self.synchronize(state);
        let index = self.table_state.selected()?;
        state
            .region
            .rail_authority
            .infrastructure_projects
            .get(index)
            .map(|project| (index, project))
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
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
        Layout::vertical([Constraint::Length(9), Constraint::Fill(1)]).areas(area);
    let [finance_area, programme_area] = Layout::horizontal([
        Constraint::Percentage(58),
        Constraint::Percentage(42),
    ])
    .spacing(1)
    .areas(summary_area);
    render_finances(frame, finance_area, state);
    render_programme(frame, programme_area, state);

    let [projects_area, inspector_area] = Layout::horizontal([
        Constraint::Percentage(58),
        Constraint::Percentage(42),
    ])
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
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Fill(1),
    ])
    .areas(area);
    render_finances(frame, summary_area, state);
    render_projects(frame, projects_area, state, now, selection, true);
    render_project_inspector(frame, inspector_area, state, now, selection);
}

fn render_tiny(frame: &mut Frame, area: Rect, state: &GameState, now: UtcSeconds) {
    frame.render_widget(
        Paragraph::new(render(state, now))
            .block(panel("Rail Authority"))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_finances(frame: &mut Frame, area: Rect, state: &GameState) {
    let authority = &state.region.rail_authority;
    let finances = &authority.finances;
    let available = finances
        .uncommitted_investment()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());

    let lines = vec![
        Line::from(vec![
            Span::styled("Authority  ", theme::secondary()),
            Span::styled(authority.name.clone(), theme::title()),
        ]),
        money_line("Treasury", finances.treasury),
        Line::from(vec![
            Span::styled("Available investment  ", theme::secondary()),
            Span::styled(available, theme::success()),
        ]),
        money_line("Maintenance reserve", finances.maintenance_reserve),
        money_line("Committed projects", finances.committed_investment),
        money_line("Regional allocation", finances.regional_public_allocation),
        money_line(
            "Access-fee revenue",
            finances.infrastructure_access_fee_revenue,
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Infrastructure Finances"))
            .style(theme::panel()),
        area,
    );
}

fn render_programme(frame: &mut Frame, area: Rect, state: &GameState) {
    let authority = &state.region.rail_authority;
    let projects = &authority.infrastructure_projects;
    let active = authority.active_construction_count();
    let reserved = authority.reserved_construction_count();
    let open = projects
        .iter()
        .filter(|project| project.status == InfrastructureProjectStatus::Open)
        .count();
    let pipeline = projects
        .iter()
        .filter(|project| {
            !matches!(
                project.status,
                InfrastructureProjectStatus::Open | InfrastructureProjectStatus::Cancelled
            )
        })
        .count();

    let lines = vec![
        Line::from(vec![
            Span::styled("Network  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} stations · {} segments",
                    authority.rail_network.rail_stations.len(),
                    authority.rail_network.rail_lines.len()
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
                    authority.construction_capacity,
                    authority.construction_slots_remaining()
                ),
                if authority.construction_slots_remaining() == 0 {
                    theme::warning()
                } else {
                    theme::primary_value()
                },
            ),
        ]),
        status_count_line(projects, InfrastructureProjectStatus::Funding, "Funding"),
        status_count_line(projects, InfrastructureProjectStatus::Scheduled, "Scheduled"),
        status_count_line(
            projects,
            InfrastructureProjectStatus::Construction,
            "Construction",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Development Programme"))
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
                    "The Authority is evaluating unconnected settlements.",
                    theme::secondary(),
                ),
            ])
            .block(panel("Infrastructure Projects"))
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
        let next = project_next(project, now);
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
                Cell::from(status),
                Cell::from(format::money(project.funding.estimated_cost)),
                Cell::from(funding_percent(project)),
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
            Row::new(["#", "Project", "Status", "Cost", "Funded", "Next"])
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
        .block(panel("Infrastructure Projects"))
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
                .block(panel("Project Details"))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let gap = project
        .funding
        .funding_gap()
        .map(format::money)
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
        Line::from(""),
        money_line("Estimated cost", project.funding.estimated_cost),
        money_line("Authority committed", project.funding.authority_committed),
        Line::from(vec![
            Span::styled("Funding gap  ", theme::secondary()),
            Span::styled(gap, theme::primary_value()),
        ]),
    ];

    append_project_scope_details(&mut lines, state, project);
    lines.push(Line::from(""));
    append_timeline(&mut lines, project, now);

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Project Details"))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
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
            lines.push(Line::styled("PLANNED INFRASTRUCTURE", theme::table_header()));
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
                Span::styled(format::distance(total_metres), theme::primary_value()),
            ]));
            if let Some(line) = planned_lines.first() {
                lines.push(Line::from(vec![
                    Span::styled("Initial capability  ", theme::secondary()),
                    Span::styled(
                        format!(
                            "{} km/h · {} track{} · {} · {} difficulty",
                            line.speed_limit.kilometres_per_hour(),
                            line.track_count.tracks(),
                            if line.track_count.tracks() == 1 { "" } else { "s" },
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
        InfrastructureProjectKind::Renewal { rail_line_ids } => {
            lines.push(Line::from(format!("Renew {} segment(s)", rail_line_ids.len())))
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => lines.push(Line::from(
            format!("Upgrade {} station(s)", rail_station_ids.len()),
        )),
    }
}

fn append_timeline(lines: &mut Vec<Line<'static>>, project: &InfrastructureProject, now: UtcSeconds) {
    lines.push(Line::styled("PROJECT TIMELINE", theme::table_header()));
    lines.push(timestamp_line("Requested", project.timeline.requested_at, now));
    if let Some(value) = project.timeline.approved_at {
        lines.push(timestamp_line("Approved", value, now));
    }
    if let Some(value) = project.timeline.funding_completed_at {
        lines.push(timestamp_line("Funded", value, now));
    }

    match (
        project.timeline.scheduled_start_at,
        project.timeline.construction_started_at,
    ) {
        (Some(scheduled), None) => lines.push(schedule_line("Scheduled start", scheduled, now)),
        (_, Some(started)) => {
            lines.push(timestamp_line("Construction started", started, now));
            if let Some(completion) = project.timeline.planned_completion_at {
                let duration = completion
                    .unix_seconds()
                    .saturating_sub(started.unix_seconds())
                    .max(0) as u64;
                lines.push(duration_line("Construction duration", duration));
                lines.push(absolute_timestamp_line("Expected opening", completion));

                let remaining = completion
                    .unix_seconds()
                    .saturating_sub(now.unix_seconds())
                    .max(0) as u64;
                lines.push(Line::from(vec![
                    Span::styled("Time remaining  ", theme::secondary()),
                    Span::styled(format::duration(remaining), theme::primary_value()),
                ]));
            }
        }
        _ => {}
    }

    if project.timeline.construction_started_at.is_none() {
        if let Some(value) = project.timeline.planned_completion_at {
            lines.push(absolute_timestamp_line("Expected opening", value));
        }
    }
    if let Some(value) = project.timeline.completed_at {
        lines.push(absolute_timestamp_line("Opened", value));
    }
    if let Some(value) = project.timeline.cancelled_at {
        lines.push(timestamp_line("Cancelled", value, now));
    }
}

fn money_line(label: &str, value: Money) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(format::money(value), theme::primary_value()),
    ])
}

fn status_count_line(
    projects: &[InfrastructureProject],
    status: InfrastructureProjectStatus,
    label: &str,
) -> Line<'static> {
    let count = projects
        .iter()
        .filter(|project| project.status == status)
        .count();
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(count.to_string(), theme::primary_value()),
    ])
}

fn project_scope(state: &GameState, project: &InfrastructureProject) -> String {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            if let Some(route) = new_line_route_label(state, planned_stations, planned_lines) {
                format!("New line · {route}")
            } else {
                let places = planned_stations
                    .iter()
                    .map(|station| settlement_name(state, station.settlement_id))
                    .collect::<Vec<_>>();
                if places.is_empty() {
                    "New line".into()
                } else {
                    format!("New line · {}", places.join(" / "))
                }
            }
        }
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. } => {
            format!("Speed upgrade · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. } => {
            format!("Double tracking · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::Electrification { rail_line_ids } => {
            format!("Electrification · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::Renewal { rail_line_ids } => {
            format!("Renewal · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            format!("Station upgrade · {} station(s)", rail_station_ids.len())
        }
    }
}

fn new_line_route_label(
    state: &GameState,
    planned_stations: &[crate::model::PlannedRailStation],
    planned_lines: &[crate::model::PlannedRailLine],
) -> Option<String> {
    let first_line = planned_lines.first()?;
    let is_planned_station = |station_id| {
        planned_stations
            .iter()
            .any(|station| station.id == station_id)
    };

    // New connection projects currently grow outward from the existing network.
    // Prefer an existing endpoint as the route origin so the UI reads naturally
    // as "Existing station → New settlement" regardless of stored endpoint order.
    let origin_id = planned_lines
        .iter()
        .flat_map(|line| [line.first_station_id, line.second_station_id])
        .find(|station_id| !is_planned_station(*station_id))
        .unwrap_or(first_line.first_station_id);

    // Prefer a planned endpoint at the edge of the planned graph. This also
    // produces a useful origin → destination label if a later NewLine project
    // contains more than one planned segment/station.
    let destination_id = planned_stations
        .iter()
        .map(|station| station.id)
        .find(|station_id| {
            planned_lines
                .iter()
                .filter(|line| {
                    line.first_station_id == *station_id || line.second_station_id == *station_id
                })
                .count()
                == 1
        })
        .or_else(|| planned_stations.last().map(|station| station.id))
        .unwrap_or(first_line.second_station_id);

    let origin = station_name(state, planned_stations, origin_id);
    let destination = station_name(state, planned_stations, destination_id);
    Some(format!("{origin} → {destination}"))
}

fn station_name(
    state: &GameState,
    planned_stations: &[crate::model::PlannedRailStation],
    station_id: crate::model::RailStationId,
) -> String {
    if let Some(station) = planned_stations.iter().find(|station| station.id == station_id) {
        return settlement_name(state, station.settlement_id);
    }

    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
        .map(|station| settlement_name(state, station.settlement_id))
        .unwrap_or_else(|| "Unknown station".into())
}

fn settlement_name(state: &GameState, settlement_id: crate::model::SettlementId) -> String {
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| "Unknown settlement".into())
}

fn project_status(status: InfrastructureProjectStatus) -> &'static str {
    match status {
        InfrastructureProjectStatus::Requested => "REQUESTED",
        InfrastructureProjectStatus::UnderReview => "UNDER REVIEW",
        InfrastructureProjectStatus::Proposed => "PROPOSED",
        InfrastructureProjectStatus::Approved => "APPROVED",
        InfrastructureProjectStatus::Deferred => "DEFERRED",
        InfrastructureProjectStatus::Funding => "FUNDING",
        InfrastructureProjectStatus::Scheduled => "SCHEDULED",
        InfrastructureProjectStatus::Construction => "CONSTRUCTION",
        InfrastructureProjectStatus::Open => "OPEN",
        InfrastructureProjectStatus::Cancelled => "CANCELLED",
    }
}

fn status_style(status: InfrastructureProjectStatus) -> ratatui::style::Style {
    match status {
        InfrastructureProjectStatus::Open => theme::success(),
        InfrastructureProjectStatus::Deferred | InfrastructureProjectStatus::Cancelled => {
            theme::error()
        }
        InfrastructureProjectStatus::Funding
        | InfrastructureProjectStatus::Scheduled
        | InfrastructureProjectStatus::Construction => theme::warning(),
        _ => theme::primary_value(),
    }
}

fn funding_percent(project: &InfrastructureProject) -> String {
    let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
    let committed = i128::from(project.funding.authority_committed.cents()).max(0);
    if estimated == 0 {
        return "—".into();
    }
    let percent = committed.saturating_mul(100).saturating_div(estimated).min(100);
    format!("{percent}%")
}

fn project_next(project: &InfrastructureProject, now: UtcSeconds) -> String {
    match project.status {
        InfrastructureProjectStatus::Requested => "Awaiting review".into(),
        InfrastructureProjectStatus::UnderReview => "Review underway".into(),
        InfrastructureProjectStatus::Proposed => "Decision pending".into(),
        InfrastructureProjectStatus::Approved => "Funding next".into(),
        InfrastructureProjectStatus::Deferred => "Deferred".into(),
        InfrastructureProjectStatus::Funding => project
            .funding
            .funding_gap()
            .map(|gap| {
                if gap <= Money::ZERO {
                    "Fully funded".into()
                } else {
                    format!("Gap {}", format::money(gap))
                }
            })
            .unwrap_or_else(|_| "Funding".into()),
        InfrastructureProjectStatus::Scheduled => project
            .timeline
            .scheduled_start_at
            .map(|value| relative_time(value, now))
            .unwrap_or_else(|| "Queued".into()),
        InfrastructureProjectStatus::Construction => project
            .timeline
            .planned_completion_at
            .map(|value| relative_time(value, now))
            .unwrap_or_else(|| "In progress".into()),
        InfrastructureProjectStatus::Open => "Open".into(),
        InfrastructureProjectStatus::Cancelled => "Cancelled".into(),
    }
}

fn timestamp_line(label: &str, timestamp: UtcSeconds, now: UtcSeconds) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(relative_time(timestamp, now), theme::primary_value()),
    ])
}

fn schedule_line(label: &str, timestamp: UtcSeconds, now: UtcSeconds) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(format_utc_timestamp(timestamp), theme::primary_value()),
        Span::styled(" · ", theme::secondary()),
        Span::styled(relative_time(timestamp, now), theme::primary_value()),
    ])
}

fn duration_line(label: &str, seconds: u64) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(format::duration(seconds), theme::primary_value()),
    ])
}

fn absolute_timestamp_line(label: &str, timestamp: UtcSeconds) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(format_utc_timestamp(timestamp), theme::primary_value()),
    ])
}

fn format_utc_timestamp(timestamp: UtcSeconds) -> String {
    let unix = timestamp.unix_seconds();
    let days = unix.div_euclid(86_400);
    let seconds_of_day = unix.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_unix_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    let shifted = days.saturating_add(719_468);
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn relative_time(timestamp: UtcSeconds, now: UtcSeconds) -> String {
    let delta = timestamp
        .unix_seconds()
        .saturating_sub(now.unix_seconds());
    if delta == 0 {
        return "now".into();
    }
    let seconds = delta.unsigned_abs();
    if delta > 0 {
        format!("in {}", format::duration(seconds))
    } else {
        format!("{} ago", format::duration(seconds))
    }
}

fn short_uuid(id: InfrastructureProjectId) -> String {
    id.to_string().chars().take(8).collect()
}

fn electrification_label(value: Electrification) -> &'static str {
    match value {
        Electrification::None => "non-electrified",
        Electrification::Electric => "electric",
    }
}

fn difficulty_label(value: ConstructionDifficulty) -> &'static str {
    match value {
        ConstructionDifficulty::Low => "low",
        ConstructionDifficulty::Moderate => "moderate",
        ConstructionDifficulty::High => "high",
    }
}

fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(theme::THIN_BORDERS)
        .border_style(theme::border())
        .title(title)
        .title_style(theme::title())
        .style(theme::panel())
}

/// Compact textual fallback used by very small terminals and tests.
pub fn render(state: &GameState, now: UtcSeconds) -> String {
    let authority = &state.region.rail_authority;
    let finances = &authority.finances;
    let available = finances
        .uncommitted_investment()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());
    let mut output = String::new();
    writeln!(output, "{}", authority.name).expect("writing to String cannot fail");
    writeln!(output, "Treasury: {}", format::money(finances.treasury))
        .expect("writing to String cannot fail");
    writeln!(output, "Available investment: {available}")
        .expect("writing to String cannot fail");
    writeln!(
        output,
        "Maintenance reserve: {}",
        format::money(finances.maintenance_reserve)
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
            project_next(project, now)
        )
        .expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::{
        model::UtcSeconds,
        sim::{authority::advance_infrastructure_planning, world::create_new_game},
    };

    use super::{ProjectSelection, format_utc_timestamp, render};

    #[test]
    fn authority_render_exposes_budget_and_project_pipeline() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let world_seed = state.world_seed;
        advance_infrastructure_planning(
            &mut state.region,
            world_seed,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();

        let output = render(&state, UtcSeconds::from_unix_seconds(0));
        assert!(output.contains("Treasury:"));
        assert!(output.contains("Available investment:"));
        assert!(output.contains("Projects:"));
        assert!(output.contains(" → "));
    }


    #[test]
    fn formats_authority_timestamps_as_exact_utc_times() {
        assert_eq!(
            format_utc_timestamp(UtcSeconds::from_unix_seconds(0)),
            "1970-01-01 00:00:00 UTC"
        );
        assert_eq!(
            format_utc_timestamp(UtcSeconds::from_unix_seconds(1_700_000_000)),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn project_selection_tracks_project_identity() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let world_seed = state.world_seed;
        advance_infrastructure_planning(
            &mut state.region,
            world_seed,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();
        let mut selection = ProjectSelection::default();
        selection.handle_key(KeyCode::Down, &state);
        assert!(selection.selected_project(&state).is_some());
    }
}

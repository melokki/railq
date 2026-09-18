//! Operational geographic map presentation.
//!
//! This module owns map layout, rail drawing, location inspection, and active
//! Train markers. The parent Map workspace owns input and selected location.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::MapLocationSelection;
use super::geometry::{
    MapCell, MapInk, can_place_text, draw_orthogonal_rail, map_ink_style, put_cell, put_text,
    rail_glyph,
};
use super::network::{format_population, panel_block, ready_trains, station_name};
use super::shared::{format_distance, format_duration, remaining_seconds};
use crate::{
    model::{
        Electrification, GameState, InfrastructureProject, InfrastructureProjectKind,
        InfrastructureProjectStatus, Journey, PassengerService, RailLineId, RailStationId,
        ServiceDirectionMode, SettlementId, TrainStatus, UtcSeconds,
    },
    sim::{
        authority::{
            COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS, local_rail_success_basis_points,
            nearest_connection_station_id,
        },
        demand::effective_arrival_rate_per_hour,
        services::{path_between_stations, service_path_for_stops},
    },
    ui::theme,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MapDirection {
    Left,
    Right,
    Up,
    Down,
}

// Most terminal cells are roughly twice as tall as they are wide. Keep this
// isolated so the visual calibration can be tuned later without touching the
// distance curve itself.
pub(super) const TERMINAL_CELL_HEIGHT_TO_WIDTH: i32 = 2;

#[derive(Clone, Debug)]
pub(super) struct OperationalLayout {
    pub(super) places: Vec<OperationalPlace>,
    lines: Vec<OperationalLine>,
}

#[derive(Clone, Debug)]
pub(super) struct OperationalPlace {
    pub(super) settlement_id: SettlementId,
    pub(super) station_id: Option<RailStationId>,
    pub(super) name: String,
    pub(super) x: i32,
    pub(super) y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InfrastructureVisualState {
    Open,
    Planned,
    Construction,
}

#[derive(Clone, Copy, Debug)]
struct OperationalLine {
    rail_line_id: RailLineId,
    first_settlement_id: SettlementId,
    second_settlement_id: SettlementId,
    distance_metres: u64,
    track_count: u8,
    electrified: bool,
    visual_state: InfrastructureVisualState,
}

/// Renders one persistent operational map containing both connected and
/// unconnected Settlements. Wide terminals keep a contextual inspector beside
/// the network; compact terminals stack it underneath without replacing the map.
pub(super) fn render_operational_map(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    selection.synchronize(state);

    let body_area = if area.height >= 5 {
        let [overview_area, body_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        render_network_overview(frame, overview_area, state);
        body_area
    } else {
        area
    };

    if body_area.width >= 92 && body_area.height >= 14 {
        let inspector_width = if body_area.width >= 120 { 40 } else { 36 };
        let [map_area, inspector_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(inspector_width)])
                .spacing(1)
                .areas(body_area);
        render_operational_network(frame, map_area, state, selection);
        render_location_inspector(frame, inspector_area, state, selection);
    } else if body_area.height >= 17 {
        let inspector_height = body_area.height.min(10);
        let [map_area, inspector_area] =
            Layout::vertical([Constraint::Min(7), Constraint::Length(inspector_height)])
                .spacing(1)
                .areas(body_area);
        render_operational_network(frame, map_area, state, selection);
        render_location_inspector(frame, inspector_area, state, selection);
    } else {
        render_operational_network(frame, body_area, state, selection);
    }
}

fn render_network_overview(frame: &mut Frame, area: Rect, state: &GameState) {
    let network = &state.region.rail_authority.rail_network;
    let station_count = network.rail_stations.len();
    let link_count = network.rail_lines.len();
    let service_count = state.player_company.passenger_services.len();
    let project_count = state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .filter(|project| project_visual_state(project.status).is_some())
        .count();
    let registration = format!(
        "{} {}",
        state.region.railway_registration.display_code(),
        state.region.railway_registration.mark
    );

    let (status, status_style) = if !state.active_journeys.is_empty() {
        ("NETWORK OPERATING", theme::focused_title())
    } else if service_count > 0 {
        ("NETWORK READY", theme::success())
    } else {
        ("NETWORK DEVELOPING", theme::warning())
    };

    let summary = if area.width >= 96 {
        format!(
            " · {station_count} stations · {link_count} rail links · {service_count} services · {project_count} projects · registration {registration}"
        )
    } else if area.width >= 68 {
        format!(
            " · {station_count} stations · {service_count} services · registration {registration}"
        )
    } else {
        format!(" · {station_count} stations · {service_count} services")
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(status, status_style.bold()),
            Span::styled(summary, theme::secondary()),
        ]))
        .style(theme::panel()),
        area,
    );
}

fn render_operational_network(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    let selected = selection.selected_settlement_id(state);
    let block = operational_network_block(area.width);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(layout) = operational_layout(state) else {
        frame.render_widget(
            Paragraph::new("No Settlements are available.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };
    if inner.width < 8 || inner.height < 4 {
        return;
    }

    let rows = render_map_rows(&layout, selected, inner.width, inner.height, state);
    frame.render_widget(
        Paragraph::new(rows)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn operational_network_block(width: u16) -> Block<'static> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::focused_border())
        .title_top(Line::styled(" Network ", theme::focused_title()).left_aligned())
        .style(theme::panel());

    // Marker meanings are spatial information, so the map keeps only this
    // compact legend. Wide maps also explain infrastructure lifecycle and
    // electrification without consuming a separate panel.
    if width >= 110 {
        block = block.title_bottom(
            Line::from(vec![
                Span::styled(" ● ", theme::primary_value()),
                Span::styled("station", theme::secondary()),
                Span::styled("   ◉ ", theme::success()),
                Span::styled("ready", theme::secondary()),
                Span::styled("   ○ ", theme::secondary()),
                Span::styled("settlement", theme::secondary()),
                Span::styled("   ─ ", theme::secondary()),
                Span::styled("single", theme::secondary()),
                Span::styled("   ═ ", theme::secondary()),
                Span::styled("double", theme::secondary()),
                Span::styled("   ─ ", theme::success()),
                Span::styled("electric", theme::secondary()),
                Span::styled("   ─ ", theme::warning()),
                Span::styled("planned", theme::secondary()),
                Span::styled("   ━ ", theme::warning().bold()),
                Span::styled("works", theme::secondary()),
                Span::styled("   ▶ ", theme::warning()),
                Span::styled("train ", theme::secondary()),
            ])
            .right_aligned(),
        );
    } else if width >= 82 {
        block = block.title_bottom(
            Line::from(vec![
                Span::styled(" ● ", theme::primary_value()),
                Span::styled("station", theme::secondary()),
                Span::styled("   ◉ ", theme::success()),
                Span::styled("ready", theme::secondary()),
                Span::styled("   ○ ", theme::secondary()),
                Span::styled("settlement", theme::secondary()),
                Span::styled("   ─ ", theme::secondary()),
                Span::styled("single", theme::secondary()),
                Span::styled("   ═ ", theme::secondary()),
                Span::styled("double", theme::secondary()),
                Span::styled("   ▶ ", theme::warning()),
                Span::styled("train ", theme::secondary()),
            ])
            .right_aligned(),
        );
    }

    block
}

fn render_location_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    let selected_id = selection.selected_settlement_id(state);
    let settlement = selected_id.and_then(|selected_id| {
        state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == selected_id)
    });
    let Some(settlement) = settlement else {
        frame.render_widget(
            Paragraph::new("No Settlement selected.")
                .block(panel_block("Inspector", false))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let station = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.settlement_id == settlement.id);
    let compact = area.height < 18;

    // The inspector follows the same hierarchy as the other redesigned
    // workspaces: selection identity first, then operational sections. Map
    // geometry and keyboard actions remain outside the inspector.
    let (panel_title, lines) = if let Some(station) = station {
        let ready_count = ready_trains(state, station.id).len();
        let arriving = state
            .active_journeys
            .iter()
            .filter(|journey| journey_next_stop_station_id(state, journey) == Some(station.id))
            .collect::<Vec<_>>();
        let next_arrival = arriving.iter().min_by_key(|journey| journey.arrives_at).copied();
        let next_arrival_summary = next_arrival
            .map(|journey| station_arrival_summary(state, journey))
            .unwrap_or_else(|| "—".into());
        let mut station_services = state
            .player_company
            .passenger_services
            .iter()
            .filter(|service| service.stop_station_ids.contains(&station.id))
            .collect::<Vec<_>>();
        station_services.sort_by(|left, right| left.name.cmp(&right.name));
        let service_count = station_services.len();
        let incident_lines = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .filter(|line| {
                line.first_station_id == station.id || line.second_station_id == station.id
            })
            .collect::<Vec<_>>();
        let single_track_count = incident_lines
            .iter()
            .filter(|line| line.track_count.tracks() == 1)
            .count();
        let double_track_count = incident_lines
            .iter()
            .filter(|line| line.track_count.tracks() >= 2)
            .count();
        let electrified_count = incident_lines
            .iter()
            .filter(|line| line.electrification == Electrification::Electric)
            .count();
        let min_speed = incident_lines
            .iter()
            .map(|line| line.speed_limit.kilometres_per_hour())
            .min();
        let max_speed = incident_lines
            .iter()
            .map(|line| line.speed_limit.kilometres_per_hour())
            .max();
        let (planned_projects, construction_projects) = station_project_counts(state, station.id);

        let mut demand = state
            .origin_destination_demand
            .iter()
            .filter(|pool| pool.origin_station_id == station.id)
            .map(|pool| {
                (
                    station_name(state, pool.destination_station_id),
                    pool.waiting_passengers,
                    effective_arrival_rate_per_hour(state, pool),
                    pool.market_maturity.basis_points(),
                )
            })
            .collect::<Vec<_>>();
        demand.sort_by_key(|(_, waiting, _, _)| Reverse(*waiting));
        let waiting_total = demand.iter().fold(0_u32, |total, (_, waiting, _, _)| {
            total.saturating_add(*waiting)
        });
        let arrival_rate_total = demand.iter().fold(0_u32, |total, (_, _, per_hour, _)| {
            total.saturating_add(*per_hour)
        });
        let average_maturity_basis_points = if demand.is_empty() {
            0
        } else {
            let total = demand.iter().fold(0_u64, |total, (_, _, _, maturity)| {
                total.saturating_add(u64::from(*maturity))
            });
            (total / demand.len() as u64) as u16
        };

        let mut lines = vec![
            inspector_selection_heading("SELECTED STATION"),
            Line::styled(settlement.name.clone(), theme::focused_title()),
            inspector_identity_line(
                &format!("Station {:02}", station.id.get()),
                &format!("Population {}", format_population(settlement.population)),
            ),
        ];

        if compact {
            lines.push(Line::from(""));
            lines.push(inspector_compact_pair(
                "Ready",
                &ready_count.to_string(),
                "Inbound",
                &arriving.len().to_string(),
            ));
            if next_arrival.is_some() {
                lines.push(inspector_metric("Next arrival", &next_arrival_summary));
            }
            lines.push(inspector_metric("Services", &service_count.to_string()));
            lines.push(inspector_compact_pair(
                "Waiting",
                &format_population(u64::from(waiting_total)),
                "Demand",
                &format!("+{arrival_rate_total}/h"),
            ));
            lines.push(inspector_metric(
                "Rail adoption",
                &market_maturity_summary(average_maturity_basis_points),
            ));
        } else {
            lines.push(Line::from(""));
            lines.push(inspector_section("TRAFFIC"));
            lines.push(inspector_metric("Ready trains", &ready_count.to_string()));
            lines.push(inspector_metric("Inbound", &arriving.len().to_string()));
            lines.push(inspector_metric("Next arrival", &next_arrival_summary));

            lines.push(Line::from(""));
            lines.push(inspector_section("SERVICES"));
            if station_services.is_empty() {
                lines.push(Line::styled("No passenger services", theme::secondary()));
            } else {
                for service in station_services.iter().take(3) {
                    lines.push(inspector_service_line(state, service, area.width));
                }
                if service_count > 3 {
                    lines.push(Line::styled(
                        format!("+{} more", service_count - 3),
                        theme::secondary(),
                    ));
                }
            }

            lines.push(Line::from(""));
            lines.push(inspector_section("PASSENGERS"));
            lines.push(inspector_metric(
                "Waiting",
                &format_population(u64::from(waiting_total)),
            ));
            lines.push(inspector_metric(
                "Arrival rate",
                &format!("+{arrival_rate_total}/h"),
            ));
            lines.push(inspector_metric(
                "Rail adoption",
                &market_maturity_summary(average_maturity_basis_points),
            ));

            if !demand.is_empty() && area.height >= 29 {
                lines.push(Line::from(""));
                lines.push(inspector_section("PASSENGER MARKETS"));
                for (name, waiting, per_hour, maturity) in demand.into_iter().take(3) {
                    lines.push(inspector_destination_line(
                        &name, waiting, per_hour, maturity,
                    ));
                }
            }

            if area.height >= 34 {
                lines.push(Line::from(""));
                lines.push(inspector_section("INFRASTRUCTURE"));
                lines.push(inspector_metric("Connections", &incident_lines.len().to_string()));
                lines.push(inspector_metric(
                    "Track",
                    &format!("{single_track_count} single · {double_track_count} double"),
                ));
                lines.push(inspector_metric(
                    "Electrified",
                    &format!("{electrified_count} / {} links", incident_lines.len()),
                ));
                lines.push(inspector_metric(
                    "Speed limit",
                    &infrastructure_speed_range(min_speed, max_speed),
                ));
                if planned_projects > 0 || construction_projects > 0 {
                    lines.push(inspector_metric(
                        "Projects",
                        &format!(
                            "{planned_projects} planned · {construction_projects} works"
                        ),
                    ));
                }
            }
        }

        ("Station", lines)
    } else {
        let mut lines = vec![
            inspector_selection_heading("SELECTED SETTLEMENT"),
            Line::styled(settlement.name.clone(), theme::focused_title()),
            Line::styled(
                format!("Population {}", format_population(settlement.population)),
                theme::secondary(),
            ),
        ];
        let council_signal = nearest_connection_station_id(&state.region, settlement.id).map(
            |connection_station_id| {
                local_rail_success_basis_points(
                    &state.origin_destination_demand,
                    connection_station_id,
                )
            },
        );
        let connection_project = connection_project_status(state, settlement.id);

        if compact {
            lines.push(Line::from(""));
            lines.push(inspector_metric(
                "Rail access",
                connection_project
                    .map(connection_project_summary)
                    .unwrap_or("No station"),
            ));
            if let Some(maturity) = council_signal {
                lines.push(inspector_metric(
                    "Council case",
                    &format!(
                        "{} / {}",
                        market_maturity_percent(maturity),
                        market_maturity_percent(COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS)
                    ),
                ));
            } else {
                lines.push(inspector_metric("Passenger rail", "Unavailable"));
            }
        } else {
            lines.push(Line::from(""));
            lines.push(inspector_section("RAIL ACCESS"));
            if let Some(status) = connection_project {
                lines.push(Line::styled(
                    connection_project_summary(status),
                    project_status_style(status),
                ));
                lines.push(inspector_metric("Project", project_status_label(status)));
                lines.push(Line::styled(
                    "Passenger services become available when the connection opens.",
                    theme::secondary(),
                ));
            } else {
                lines.push(Line::styled("No rail station", theme::primary_value()));
                lines.push(Line::styled(
                    "Passenger services require a connection to the rail network.",
                    theme::secondary(),
                ));

                if let Some(maturity) = council_signal {
                    lines.push(Line::from(""));
                    lines.push(inspector_section("COUNCIL CONNECTION CASE"));
                    lines.push(inspector_metric(
                        "Nearby adoption",
                        &market_maturity_summary(maturity),
                    ));
                    lines.push(inspector_metric(
                        "Request threshold",
                        &market_maturity_percent(COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS),
                    ));
                    lines.push(Line::styled(
                        if maturity >= COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS {
                            "Local rail success is strong enough for a council connection request."
                        } else {
                            "Nearby rail use must grow before the council can request a connection."
                        },
                        theme::secondary(),
                    ));
                }
            }
        }

        ("Settlement", lines)
    };

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(panel_title, false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn inspector_selection_heading(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::secondary().bold())
}

fn inspector_identity_line(first: &str, second: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(first.to_owned(), theme::primary_value()),
        Span::styled(" · ", theme::secondary()),
        Span::styled(second.to_owned(), theme::secondary()),
    ])
}

fn inspector_compact_pair(
    first_label: &str,
    first_value: &str,
    second_label: &str,
    second_value: &str,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{first_label} "), theme::secondary()),
        Span::styled(first_value.to_owned(), theme::primary_value()),
        Span::styled(" · ", theme::secondary()),
        Span::styled(format!("{second_label} "), theme::secondary()),
        Span::styled(second_value.to_owned(), theme::primary_value()),
    ])
}

fn inspector_section(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::secondary().bold())
}

fn inspector_metric(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<15}"), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn inspector_service_line(
    state: &GameState,
    service: &PassengerService,
    panel_width: u16,
) -> Line<'static> {
    let origin = service
        .origin_station_id()
        .map(|station_id| station_name(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let destination = service
        .destination_station_id()
        .map(|station_id| station_name(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let separator = match service.direction_mode {
        ServiceDirectionMode::BothDirections => " ↔ ",
        ServiceDirectionMode::ForwardOnly => " → ",
    };
    let route = format!("{origin}{separator}{destination}");
    let route_width = usize::from(panel_width.saturating_sub(9).max(1));

    Line::from(vec![
        Span::styled(format!("{:<5}", service.name), theme::primary_value()),
        Span::styled(truncate_label(&route, route_width), theme::secondary()),
    ])
}

fn station_arrival_summary(state: &GameState, journey: &Journey) -> String {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .map(|service| service.name.as_str())
        .unwrap_or("—");
    format!(
        "Train {:02} · {service} · {}",
        journey.train_id.get(),
        format_duration(remaining_seconds(journey, state.last_processed_at))
    )
}

fn inspector_destination_line(
    name: &str,
    waiting: u32,
    per_hour: u32,
    maturity_basis_points: u16,
) -> Line<'static> {
    let name = truncate_label(name, 13);
    Line::from(vec![
        Span::styled(format!("→ {name:<13}"), theme::secondary()),
        Span::styled(
            format!(
                "{waiting} · +{per_hour}/h · {}",
                market_maturity_percent(maturity_basis_points)
            ),
            theme::primary_value(),
        ),
    ])
}

fn market_maturity_summary(basis_points: u16) -> String {
    format!(
        "{} · {}",
        market_maturity_percent(basis_points),
        market_maturity_label(basis_points)
    )
}

pub(super) fn market_maturity_percent(basis_points: u16) -> String {
    format!("{}%", u32::from(basis_points).saturating_add(50) / 100)
}

fn market_maturity_label(basis_points: u16) -> &'static str {
    match basis_points {
        0..=3_499 => "Emerging",
        3_500..=5_999 => "Growing",
        6_000..=8_499 => "Established",
        _ => "Mature",
    }
}

fn infrastructure_speed_range(min_speed: Option<u16>, max_speed: Option<u16>) -> String {
    match (min_speed, max_speed) {
        (Some(minimum), Some(maximum)) if minimum != maximum => {
            format!("{minimum}–{maximum} km/h")
        }
        (Some(speed), _) => format!("{speed} km/h"),
        _ => "—".into(),
    }
}

fn station_project_counts(state: &GameState, station_id: RailStationId) -> (usize, usize) {
    let mut planned = 0_usize;
    let mut construction = 0_usize;
    for project in &state.region.rail_authority.infrastructure_projects {
        let Some(visual_state) = project_visual_state(project.status) else {
            continue;
        };
        if !project_affects_station(state, project, station_id) {
            continue;
        }
        match visual_state {
            InfrastructureVisualState::Planned => planned = planned.saturating_add(1),
            InfrastructureVisualState::Construction => {
                construction = construction.saturating_add(1)
            }
            InfrastructureVisualState::Open => {}
        }
    }
    (planned, construction)
}

fn project_affects_station(
    state: &GameState,
    project: &InfrastructureProject,
    station_id: RailStationId,
) -> bool {
    match &project.kind {
        InfrastructureProjectKind::NewLine { planned_lines, .. } => planned_lines.iter().any(|line| {
            line.first_station_id == station_id || line.second_station_id == station_id
        }),
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
        | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
        | InfrastructureProjectKind::Electrification { rail_line_ids }
        | InfrastructureProjectKind::Renewal { rail_line_ids } => state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .any(|line| {
                rail_line_ids.contains(&line.id)
                    && (line.first_station_id == station_id
                        || line.second_station_id == station_id)
            }),
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            rail_station_ids.contains(&station_id)
        }
    }
}

fn connection_project_status(
    state: &GameState,
    settlement_id: SettlementId,
) -> Option<InfrastructureProjectStatus> {
    state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .filter(|project| project_visual_state(project.status).is_some())
        .filter(|project| {
            matches!(
                &project.kind,
                InfrastructureProjectKind::NewLine { planned_stations, .. }
                    if planned_stations
                        .iter()
                        .any(|station| station.settlement_id == settlement_id)
            )
        })
        .max_by_key(|project| {
            project_visual_state(project.status)
                .map(infrastructure_visual_priority)
                .unwrap_or(0)
        })
        .map(|project| project.status)
}

fn connection_project_summary(status: InfrastructureProjectStatus) -> &'static str {
    match project_visual_state(status) {
        Some(InfrastructureVisualState::Construction) => "Station under construction",
        Some(InfrastructureVisualState::Planned) => "Connection planned",
        Some(InfrastructureVisualState::Open) | None => "No station",
    }
}

fn project_status_label(status: InfrastructureProjectStatus) -> &'static str {
    match status {
        InfrastructureProjectStatus::Proposed => "PROPOSED",
        InfrastructureProjectStatus::Approved => "APPROVED",
        InfrastructureProjectStatus::Funding => "FUNDING",
        InfrastructureProjectStatus::Scheduled => "SCHEDULED",
        InfrastructureProjectStatus::Construction => "CONSTRUCTION",
        InfrastructureProjectStatus::Requested => "REQUESTED",
        InfrastructureProjectStatus::UnderReview => "UNDER REVIEW",
        InfrastructureProjectStatus::Deferred => "DEFERRED",
        InfrastructureProjectStatus::Rejected => "REJECTED",
        InfrastructureProjectStatus::Open => "OPEN",
        InfrastructureProjectStatus::Cancelled => "CANCELLED",
    }
}

fn project_status_style(status: InfrastructureProjectStatus) -> ratatui::style::Style {
    match project_visual_state(status) {
        Some(InfrastructureVisualState::Construction) => theme::warning().bold(),
        Some(InfrastructureVisualState::Planned) => theme::warning(),
        Some(InfrastructureVisualState::Open) | None => theme::primary_value(),
    }
}

fn truncate_label(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let prefix = characters.by_ref().take(max_chars).collect::<String>();
    if characters.next().is_some() && max_chars > 1 {
        let mut shortened = prefix.chars().take(max_chars - 1).collect::<String>();
        shortened.push('…');
        shortened
    } else {
        prefix
    }
}

pub(super) fn operational_layout(state: &GameState) -> Option<OperationalLayout> {
    let network = &state.region.rail_authority.rail_network;
    if state.region.settlements.is_empty() {
        return None;
    }

    let station_by_id = network
        .rail_stations
        .iter()
        .map(|station| (station.id, station))
        .collect::<BTreeMap<_, _>>();
    let station_by_settlement = network
        .rail_stations
        .iter()
        .map(|station| (station.settlement_id, station.id))
        .collect::<BTreeMap<_, _>>();

    let places = state
        .region
        .settlements
        .iter()
        .map(|settlement| OperationalPlace {
            settlement_id: settlement.id,
            station_id: station_by_settlement.get(&settlement.id).copied(),
            name: settlement.name.clone(),
            x: settlement.position.x,
            // Terminal cells are roughly twice as tall as they are wide.
            // Compress world-space Y only for presentation so geography stays
            // visually proportional while the persisted coordinates remain
            // simulation-grade kilometres.
            y: settlement
                .position
                .y
                .div_euclid(TERMINAL_CELL_HEIGHT_TO_WIDTH),
        })
        .collect::<Vec<_>>();

    let project_line_states = project_line_visual_states(state);
    let mut lines = network
        .rail_lines
        .iter()
        .filter_map(|line| {
            let first = station_by_id.get(&line.first_station_id)?;
            let second = station_by_id.get(&line.second_station_id)?;
            Some(OperationalLine {
                rail_line_id: line.id,
                first_settlement_id: first.settlement_id,
                second_settlement_id: second.settlement_id,
                distance_metres: line.distance.metres(),
                track_count: line.track_count.tracks(),
                electrified: line.electrification == Electrification::Electric,
                visual_state: project_line_states
                    .get(&line.id)
                    .copied()
                    .unwrap_or(InfrastructureVisualState::Open),
            })
        })
        .collect::<Vec<_>>();

    let mut station_settlements = station_by_id
        .iter()
        .map(|(station_id, station)| (*station_id, station.settlement_id))
        .collect::<BTreeMap<_, _>>();
    for project in &state.region.rail_authority.infrastructure_projects {
        let Some(visual_state) = project_visual_state(project.status) else {
            continue;
        };
        let InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } = &project.kind
        else {
            continue;
        };

        for station in planned_stations {
            station_settlements.insert(station.id, station.settlement_id);
        }
        for line in planned_lines {
            let (Some(first_settlement_id), Some(second_settlement_id)) = (
                station_settlements.get(&line.first_station_id).copied(),
                station_settlements.get(&line.second_station_id).copied(),
            ) else {
                continue;
            };
            lines.push(OperationalLine {
                rail_line_id: line.id,
                first_settlement_id,
                second_settlement_id,
                distance_metres: line.distance.metres(),
                track_count: line.track_count.tracks(),
                electrified: line.electrification == Electrification::Electric,
                visual_state,
            });
        }
    }

    Some(OperationalLayout { places, lines })
}

fn project_visual_state(status: InfrastructureProjectStatus) -> Option<InfrastructureVisualState> {
    match status {
        InfrastructureProjectStatus::Proposed
        | InfrastructureProjectStatus::Approved
        | InfrastructureProjectStatus::Funding
        | InfrastructureProjectStatus::Scheduled => Some(InfrastructureVisualState::Planned),
        InfrastructureProjectStatus::Construction => Some(InfrastructureVisualState::Construction),
        InfrastructureProjectStatus::Requested
        | InfrastructureProjectStatus::UnderReview
        | InfrastructureProjectStatus::Deferred
        | InfrastructureProjectStatus::Rejected
        | InfrastructureProjectStatus::Open
        | InfrastructureProjectStatus::Cancelled => None,
    }
}

fn project_line_visual_states(
    state: &GameState,
) -> BTreeMap<RailLineId, InfrastructureVisualState> {
    let mut result = BTreeMap::new();
    for project in &state.region.rail_authority.infrastructure_projects {
        let Some(visual_state) = project_visual_state(project.status) else {
            continue;
        };
        let Some(line_ids) = project_existing_line_ids(project) else {
            continue;
        };
        for rail_line_id in line_ids {
            let current = result
                .get(rail_line_id)
                .copied()
                .unwrap_or(InfrastructureVisualState::Open);
            if infrastructure_visual_priority(visual_state) > infrastructure_visual_priority(current) {
                result.insert(*rail_line_id, visual_state);
            }
        }
    }
    result
}

fn project_existing_line_ids(project: &InfrastructureProject) -> Option<&[RailLineId]> {
    match &project.kind {
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
        | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
        | InfrastructureProjectKind::Electrification { rail_line_ids }
        | InfrastructureProjectKind::Renewal { rail_line_ids } => Some(rail_line_ids),
        InfrastructureProjectKind::NewLine { .. }
        | InfrastructureProjectKind::StationUpgrade { .. } => None,
    }
}

fn infrastructure_visual_priority(state: InfrastructureVisualState) -> u8 {
    match state {
        InfrastructureVisualState::Open => 0,
        InfrastructureVisualState::Planned => 1,
        InfrastructureVisualState::Construction => 2,
    }
}

fn settlement_project_visual_state(
    state: &GameState,
    settlement_id: SettlementId,
    station_id: Option<RailStationId>,
) -> Option<InfrastructureVisualState> {
    let mut strongest = None;
    for project in &state.region.rail_authority.infrastructure_projects {
        let Some(visual_state) = project_visual_state(project.status) else {
            continue;
        };
        let affects_location = match &project.kind {
            InfrastructureProjectKind::NewLine {
                planned_stations,
                planned_lines,
            } => {
                planned_stations
                    .iter()
                    .any(|planned| planned.settlement_id == settlement_id)
                    || station_id.is_some_and(|station_id| {
                        planned_lines.iter().any(|line| {
                            line.first_station_id == station_id
                                || line.second_station_id == station_id
                        })
                    })
            }
            InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
                station_id.is_some_and(|station_id| rail_station_ids.contains(&station_id))
            }
            InfrastructureProjectKind::SpeedUpgrade { .. }
            | InfrastructureProjectKind::DoubleTracking { .. }
            | InfrastructureProjectKind::Electrification { .. }
            | InfrastructureProjectKind::Renewal { .. } => false,
        };
        if !affects_location {
            continue;
        }
        let replace = match strongest {
            Some(current) => {
                infrastructure_visual_priority(visual_state)
                    > infrastructure_visual_priority(current)
            }
            None => true,
        };
        if replace {
            strongest = Some(visual_state);
        }
    }
    strongest
}

#[derive(Clone, Debug, Default)]
struct ServiceRoutePreviewOverlay {
    route_line_ids: BTreeSet<RailLineId>,
    stop_order: BTreeMap<RailStationId, usize>,
    highlighted_station_id: Option<RailStationId>,
}

const SERVICE_PREVIEW_CONTEXT_LIMIT: usize = 16;

/// Build a route-focused slice of the operational map for the Service editor.
///
/// The full Map can eventually contain hundreds of stations, while the editor
/// only needs enough surrounding infrastructure to keep the route oriented.
/// Keep every station that belongs to the selected/candidate path, then add a
/// bounded one-hop neighbourhood around it. This preserves the same persisted
/// coordinates and rail geometry without shrinking the whole Region into a
/// tiny rectangle.
fn service_preview_layout(
    layout: &OperationalLayout,
    overlay: &ServiceRoutePreviewOverlay,
) -> OperationalLayout {
    let station_to_settlement = layout
        .places
        .iter()
        .filter_map(|place| {
            place
                .station_id
                .map(|station_id| (station_id, place.settlement_id))
        })
        .collect::<BTreeMap<_, _>>();

    let mut focus = BTreeSet::new();
    for station_id in overlay.stop_order.keys().copied() {
        if let Some(settlement_id) = station_to_settlement.get(&station_id) {
            focus.insert(*settlement_id);
        }
    }
    if let Some(station_id) = overlay.highlighted_station_id {
        if let Some(settlement_id) = station_to_settlement.get(&station_id) {
            focus.insert(*settlement_id);
        }
    }
    for line in layout
        .lines
        .iter()
        .filter(|line| overlay.route_line_ids.contains(&line.rail_line_id))
    {
        focus.insert(line.first_settlement_id);
        focus.insert(line.second_settlement_id);
    }

    if focus.is_empty() {
        return OperationalLayout {
            places: layout.places.clone(),
            lines: layout
                .lines
                .iter()
                .filter(|line| line.visual_state == InfrastructureVisualState::Open)
                .copied()
                .collect(),
        };
    }

    let highlighted_settlement_id = overlay
        .highlighted_station_id
        .and_then(|station_id| station_to_settlement.get(&station_id).copied());
    let by_id = layout
        .places
        .iter()
        .map(|place| (place.settlement_id, place))
        .collect::<BTreeMap<_, _>>();

    let mut context_candidates = BTreeSet::new();
    for line in layout
        .lines
        .iter()
        .filter(|line| line.visual_state == InfrastructureVisualState::Open)
    {
        if focus.contains(&line.first_settlement_id) && !focus.contains(&line.second_settlement_id)
        {
            context_candidates.insert(line.second_settlement_id);
        }
        if focus.contains(&line.second_settlement_id) && !focus.contains(&line.first_settlement_id)
        {
            context_candidates.insert(line.first_settlement_id);
        }
    }

    // When a long route has many branches, keep context closest to the current
    // cursor. Route stations themselves are never dropped.
    let mut context_candidates = context_candidates.into_iter().collect::<Vec<_>>();
    context_candidates.sort_by_key(|settlement_id| {
        let Some(place) = by_id.get(settlement_id) else {
            return (i64::MAX, *settlement_id);
        };
        let distance = highlighted_settlement_id
            .and_then(|highlighted_id| by_id.get(&highlighted_id))
            .map(|highlighted| {
                i64::from((place.x - highlighted.x).abs())
                    + i64::from((place.y - highlighted.y).abs())
            })
            .unwrap_or(0);
        (distance, *settlement_id)
    });

    let mut visible = focus;
    visible.extend(
        context_candidates
            .into_iter()
            .take(SERVICE_PREVIEW_CONTEXT_LIMIT),
    );

    let places = layout
        .places
        .iter()
        .filter(|place| visible.contains(&place.settlement_id))
        .cloned()
        .collect::<Vec<_>>();
    let lines = layout
        .lines
        .iter()
        .filter(|line| {
            line.visual_state == InfrastructureVisualState::Open
                && visible.contains(&line.first_settlement_id)
                && visible.contains(&line.second_settlement_id)
        })
        .copied()
        .collect::<Vec<_>>();

    OperationalLayout { places, lines }
}

/// Renders the Passenger Service editor preview with the same world-space
/// topology, orthogonal rail geometry, station markers, and settlement labels
/// as the main operational Map. The editor only changes emphasis: the selected
/// route is accented and the list cursor becomes the active map marker.
pub(crate) fn render_service_route_preview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    stop_station_ids: &[RailStationId],
    highlighted_station_id: Option<RailStationId>,
) {
    if area.width < 8 || area.height < 4 {
        return;
    }

    let Some(layout) = operational_layout(state) else {
        frame.render_widget(
            Paragraph::new("No Settlements are available.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let network = &state.region.rail_authority.rail_network;
    let mut route_stops = stop_station_ids.to_vec();
    if let Some(station_id) = highlighted_station_id.filter(|id| !route_stops.contains(id)) {
        let mut candidate = route_stops.clone();
        candidate.push(station_id);
        if candidate.len() >= 2 && service_path_for_stops(network, &candidate).is_ok() {
            route_stops = candidate;
        }
    }

    let route_line_ids = if route_stops.len() >= 2 {
        service_path_for_stops(network, &route_stops)
            .unwrap_or_default()
            .into_iter()
            .collect()
    } else {
        BTreeSet::new()
    };
    let overlay = ServiceRoutePreviewOverlay {
        route_line_ids,
        stop_order: stop_station_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(index, station_id)| (station_id, index + 1))
            .collect(),
        highlighted_station_id,
    };
    let preview_layout = service_preview_layout(&layout, &overlay);
    let selected_settlement_id = highlighted_station_id.and_then(|station_id| {
        preview_layout
            .places
            .iter()
            .find(|place| place.station_id == Some(station_id))
            .map(|place| place.settlement_id)
    });

    let rows = render_map_rows_with_overlay(
        &preview_layout,
        selected_settlement_id,
        area.width,
        area.height,
        state,
        Some(&overlay),
    );
    frame.render_widget(
        Paragraph::new(rows)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_map_rows(
    layout: &OperationalLayout,
    selected: Option<SettlementId>,
    width: u16,
    height: u16,
    state: &GameState,
) -> Vec<Line<'static>> {
    render_map_rows_with_overlay(layout, selected, width, height, state, None)
}

fn render_map_rows_with_overlay(
    layout: &OperationalLayout,
    selected: Option<SettlementId>,
    width: u16,
    height: u16,
    state: &GameState,
    overlay: Option<&ServiceRoutePreviewOverlay>,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let height = usize::from(height);
    let mut grid = vec![vec![MapCell::default(); width]; height];
    let mut rail_mask = vec![vec![0_u8; width]; height];
    let mut rail_ink = vec![vec![MapInk::Rail; width]; height];
    let mut rail_double = vec![vec![false; width]; height];

    let min_x = layout.places.iter().map(|place| place.x).min().unwrap_or(0) - 2;
    let max_x = layout.places.iter().map(|place| place.x).max().unwrap_or(0) + 2;
    let min_y = layout.places.iter().map(|place| place.y).min().unwrap_or(0) - 2;
    let max_y = layout.places.iter().map(|place| place.y).max().unwrap_or(0) + 2;
    let logical_width = (max_x - min_x).max(1) as f64;
    let logical_height = (max_y - min_y).max(1) as f64;
    let width_scale = if width <= 2 {
        1.0
    } else {
        (width - 1) as f64 / logical_width
    };
    let height_scale = if height <= 2 {
        1.0
    } else {
        (height - 1) as f64 / logical_height
    };
    // Use one scale for both axes. The logical layout has already compensated
    // for terminal-cell aspect ratio, so stretching X and Y independently
    // would make the same route distance look different by orientation.
    let map_scale = width_scale.min(height_scale).min(1.30);
    let x_scale = map_scale;
    let y_scale = map_scale;
    let scaled_width = (logical_width * x_scale).round() as i32;
    let scaled_height = (logical_height * y_scale).round() as i32;
    let x_padding = ((i32::try_from(width).unwrap_or(i32::MAX) - scaled_width) / 2).max(0);
    let y_padding = ((i32::try_from(height).unwrap_or(i32::MAX) - scaled_height) / 2).max(0);

    let screen_position = |place: &OperationalPlace| {
        let x = (((place.x - min_x) as f64) * x_scale).round() as i32 + x_padding;
        let y = (((place.y - min_y) as f64) * y_scale).round() as i32 + y_padding;
        (x, y)
    };
    let by_id = layout
        .places
        .iter()
        .map(|place| (place.settlement_id, place))
        .collect::<BTreeMap<_, _>>();
    let adjacent = selected_neighbours(layout, selected);

    for line in &layout.lines {
        let (Some(first), Some(second)) = (
            by_id.get(&line.first_settlement_id),
            by_id.get(&line.second_settlement_id),
        ) else {
            continue;
        };
        let start = screen_position(first);
        let end = screen_position(second);
        let accent = overlay.map_or_else(
            || {
                selected.is_some_and(|selected_id| {
                    selected_id == line.first_settlement_id
                        || selected_id == line.second_settlement_id
                })
            },
            |overlay| overlay.route_line_ids.contains(&line.rail_line_id),
        );
        let ink = if accent {
            MapInk::RailAccent
        } else {
            match line.visual_state {
                InfrastructureVisualState::Construction => MapInk::RailConstruction,
                InfrastructureVisualState::Planned => MapInk::RailPlanned,
                InfrastructureVisualState::Open if line.electrified => MapInk::RailElectric,
                InfrastructureVisualState::Open => MapInk::Rail,
            }
        };
        draw_orthogonal_rail(
            &mut rail_mask,
            &mut rail_ink,
            &mut rail_double,
            start,
            end,
            ink,
            line.track_count >= 2,
        );
    }

    for y in 0..height {
        for x in 0..width {
            if rail_mask[y][x] != 0 {
                let ink = rail_ink[y][x];
                grid[y][x] = MapCell {
                    ch: rail_glyph(
                        rail_mask[y][x],
                        matches!(ink, MapInk::RailAccent | MapInk::RailConstruction),
                        rail_double[y][x],
                    ),
                    ink,
                };
            }
        }
    }

    // Draw place markers before moving Trains. A READY Train is persistent
    // operational state, so its station marker remains visible even after an
    // arrival notice disappears. Selection keeps precedence over READY state;
    // the inspector still exposes the selected station's READY count.
    let ready_station_ids = ready_station_ids(state);
    for place in &layout.places {
        let (x, y) = screen_position(place);
        let is_ready_station = place
            .station_id
            .is_some_and(|station_id| ready_station_ids.contains(&station_id));
        let preview_stop = overlay.is_some_and(|overlay| {
            place
                .station_id
                .is_some_and(|station_id| overlay.stop_order.contains_key(&station_id))
        });
        let preview_cursor =
            overlay.is_some_and(|overlay| place.station_id == overlay.highlighted_station_id);
        let preview_stop_order = overlay.and_then(|overlay| {
            place
                .station_id
                .and_then(|station_id| overlay.stop_order.get(&station_id).copied())
        });
        let preview_last_stop = overlay
            .and_then(|overlay| overlay.stop_order.values().copied().max())
            .is_some_and(|last_stop| preview_stop_order == Some(last_stop));
        let project_visual_state = overlay
            .is_none()
            .then(|| {
                settlement_project_visual_state(state, place.settlement_id, place.station_id)
            })
            .flatten();
        let (marker, ink) = if preview_cursor {
            ('◆', MapInk::Cursor)
        } else if preview_stop_order == Some(1) {
            ('◉', MapInk::Selected)
        } else if preview_last_stop {
            ('◆', MapInk::Selected)
        } else if preview_stop {
            ('●', MapInk::Selected)
        } else if selected == Some(place.settlement_id) {
            ('◆', MapInk::Selected)
        } else if project_visual_state == Some(InfrastructureVisualState::Construction) {
            ('◆', MapInk::ProjectConstruction)
        } else if overlay.is_none() && is_ready_station {
            ('◉', MapInk::Ready)
        } else if project_visual_state == Some(InfrastructureVisualState::Planned) {
            ('◇', MapInk::ProjectPlanned)
        } else if overlay.is_some() && place.station_id.is_some() {
            ('●', MapInk::Unconnected)
        } else if place.station_id.is_some() {
            ('●', place_ink(place, selected, &adjacent))
        } else {
            ('○', place_ink(place, selected, &adjacent))
        };
        put_cell(&mut grid, x, y, marker, ink);
    }

    let place_positions = layout
        .places
        .iter()
        .map(|place| (place.settlement_id, screen_position(place)))
        .collect::<BTreeMap<_, _>>();
    let station_positions = layout
        .places
        .iter()
        .filter_map(|place| {
            place
                .station_id
                .map(|station_id| (station_id, screen_position(place)))
        })
        .collect::<BTreeMap<_, _>>();
    if overlay.is_none() {
        draw_active_train_markers(
            &mut grid,
            state,
            &station_positions,
            state.last_processed_at,
        );
    }

    // Labels follow the same focus hierarchy as the markers. Place the current
    // focus first, then its direct neighbours, so background labels cannot steal
    // the best collision-free position from the context the player is operating.
    // Operational counts live in the inspector instead of being repeated beside
    // every station name.
    let mut label_places = layout.places.iter().collect::<Vec<_>>();
    label_places.sort_by_key(|place| {
        if overlay.is_some_and(|overlay| place.station_id == overlay.highlighted_station_id) {
            0
        } else if overlay.is_some_and(|overlay| {
            place
                .station_id
                .is_some_and(|station_id| overlay.stop_order.contains_key(&station_id))
        }) {
            1
        } else {
            focus_rank(place, selected, &adjacent).saturating_add(2)
        }
    });
    for place in label_places {
        let (x, y) = screen_position(place);
        let stop_number = overlay.and_then(|overlay| {
            place
                .station_id
                .and_then(|station_id| overlay.stop_order.get(&station_id).copied())
        });
        let preview_cursor =
            overlay.is_some_and(|overlay| place.station_id == overlay.highlighted_station_id);

        // The service editor is smaller than the full Map. Keep the exact same
        // topology, but reserve scarce label space for Rail Stations and the
        // route being assembled. Unconnected settlements remain visible as
        // markers, preserving geographic context without crowding the route.
        if overlay.is_some() && place.station_id.is_none() {
            continue;
        }

        // Stop order is already visible in the picker. Prefixing route labels
        // with numbers wastes scarce horizontal space and makes close stations
        // overwrite one another, so the mini-map keeps clean station names.
        let label = map_place_label(place, selected);
        let preferred_direction =
            preferred_label_direction(place, &label, &layout.places, &place_positions, selected);
        let project_visual_state = overlay
            .is_none()
            .then(|| {
                settlement_project_visual_state(state, place.settlement_id, place.station_id)
            })
            .flatten();
        let ink = if preview_cursor {
            MapInk::Cursor
        } else if stop_number.is_some() {
            MapInk::Selected
        } else if overlay.is_some() {
            MapInk::Unconnected
        } else if selected == Some(place.settlement_id) {
            MapInk::Selected
        } else if project_visual_state == Some(InfrastructureVisualState::Construction) {
            MapInk::ProjectConstruction
        } else if project_visual_state == Some(InfrastructureVisualState::Planned) {
            MapInk::ProjectPlanned
        } else {
            place_ink(place, selected, &adjacent)
        };

        if overlay.is_some() && !preview_cursor && stop_number.is_none() {
            // Background station labels are useful context, but never at the
            // cost of overwriting rail geometry or an important route label.
            try_place_map_label(&mut grid, x, y, &label, ink, preferred_direction);
        } else if overlay.is_some() {
            // The compact Service preview must never let two important labels
            // overwrite each other. Search a slightly wider ring around the
            // station marker; the route strip below the map remains the exact
            // ordered fallback if the map is genuinely too dense to label all
            // route stations at once.
            try_place_service_preview_label(&mut grid, x, y, &label, ink, preferred_direction);
        } else {
            place_map_label(&mut grid, x, y, &label, ink, preferred_direction);
        }
    }

    // Exact distances appear only for Rail Links incident to the current
    // selection. If every candidate would collide with a place label, Train,
    // or rail geometry, omit the map annotation; the inspector still carries
    // the exact distance.
    if overlay.is_none() {
        if let Some(selected_id) = selected {
            for line in layout.lines.iter().filter(|line| {
                line.first_settlement_id == selected_id || line.second_settlement_id == selected_id
            }) {
                let (Some(first), Some(second)) = (
                    by_id.get(&line.first_settlement_id),
                    by_id.get(&line.second_settlement_id),
                ) else {
                    continue;
                };
                place_link_distance_label(
                    &mut grid,
                    screen_position(first),
                    screen_position(second),
                    line.distance_metres,
                );
            }
        }
    }

    grid.into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            let mut current_ink = MapInk::Empty;
            let mut current = String::new();
            for cell in row {
                if !current.is_empty() && cell.ink != current_ink {
                    spans.push(Span::styled(current, map_ink_style(current_ink)));
                    current = String::new();
                }
                current_ink = cell.ink;
                current.push(cell.ch);
            }
            if !current.is_empty() {
                spans.push(Span::styled(current, map_ink_style(current_ink)));
            }
            Line::from(spans)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JourneyRouteSegment {
    rail_line_id: crate::model::RailLineId,
    pub(super) from_station_id: RailStationId,
    pub(super) to_station_id: RailStationId,
    distance_metres: u64,
}

fn draw_active_train_markers(
    grid: &mut [Vec<MapCell>],
    state: &GameState,
    station_positions: &BTreeMap<RailStationId, (i32, i32)>,
    now: UtcSeconds,
) {
    let mut markers = BTreeMap::<(i32, i32), (char, usize)>::new();
    for journey in &state.active_journeys {
        let Some((position, glyph)) = journey_map_marker(state, journey, station_positions, now)
        else {
            continue;
        };
        markers
            .entry(position)
            .and_modify(|(_, count)| *count = count.saturating_add(1))
            .or_insert((glyph, 1));
    }

    for (position, (direction, count)) in markers {
        let glyph = match count {
            1 => direction,
            2..=9 => char::from_digit(u32::try_from(count).unwrap_or(9), 10).unwrap_or('+'),
            _ => '+',
        };
        put_cell(grid, position.0, position.1, glyph, MapInk::Train);
    }
}

fn journey_map_marker(
    state: &GameState,
    journey: &Journey,
    station_positions: &BTreeMap<RailStationId, (i32, i32)>,
    now: UtcSeconds,
) -> Option<((i32, i32), char)> {
    let segments = journey_route_segments(state, journey)?;
    let total_distance = segments.iter().try_fold(0_u64, |total, segment| {
        total.checked_add(segment.distance_metres)
    })?;
    if total_distance == 0 {
        return None;
    }

    let total_seconds = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .max(1);
    let elapsed_seconds = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, total_seconds);
    let travelled_metres = total_distance as f64 * (elapsed_seconds as f64 / total_seconds as f64);

    let mut distance_before = 0.0_f64;
    for (index, segment) in segments.iter().enumerate() {
        let segment_distance = segment.distance_metres as f64;
        let distance_after = distance_before + segment_distance;
        if travelled_metres <= distance_after || index + 1 == segments.len() {
            let local_progress = if segment_distance <= f64::EPSILON {
                0.0
            } else {
                ((travelled_metres - distance_before) / segment_distance).clamp(0.0, 1.0)
            };
            return journey_segment_map_marker(state, segment, station_positions, local_progress);
        }
        distance_before = distance_after;
    }

    None
}

fn journey_segment_map_marker(
    state: &GameState,
    segment: &JourneyRouteSegment,
    station_positions: &BTreeMap<RailStationId, (i32, i32)>,
    local_progress: f64,
) -> Option<((i32, i32), char)> {
    let line = state
        .region
        .rail_authority
        .rail_network
        .rail_lines
        .iter()
        .find(|line| line.id == segment.rail_line_id)?;
    let canonical_start = station_positions.get(&line.first_station_id).copied()?;
    let canonical_end = station_positions.get(&line.second_station_id).copied()?;

    let travelling_forward = segment.from_station_id == line.first_station_id
        && segment.to_station_id == line.second_station_id;
    let travelling_reverse = segment.from_station_id == line.second_station_id
        && segment.to_station_id == line.first_station_id;
    if !travelling_forward && !travelling_reverse {
        return None;
    }

    // Rail geometry is rendered from the Rail Line's persisted first endpoint
    // to its second endpoint. Reversing the Journey must therefore reverse
    // progress over that same L-shaped polyline rather than constructing a new
    // L with the opposite corner. Otherwise a reverse-running Train appears to
    // cut across empty map cells instead of following the visible Rail Line.
    Some(point_along_rendered_rail(
        canonical_start,
        canonical_end,
        travelling_forward,
        local_progress,
    ))
}

pub(super) fn point_along_rendered_rail(
    canonical_start: (i32, i32),
    canonical_end: (i32, i32),
    travelling_forward: bool,
    progress: f64,
) -> ((i32, i32), char) {
    let rendered_progress = if travelling_forward {
        progress
    } else {
        1.0 - progress
    };
    let (position, glyph) =
        point_along_orthogonal_rail(canonical_start, canonical_end, rendered_progress);
    let glyph = if travelling_forward {
        glyph
    } else {
        reverse_train_glyph(glyph)
    };
    (position, glyph)
}

fn reverse_train_glyph(glyph: char) -> char {
    match glyph {
        '▶' => '◀',
        '◀' => '▶',
        '▼' => '▲',
        '▲' => '▼',
        other => other,
    }
}

pub(super) fn journey_route_segments(
    state: &GameState,
    journey: &Journey,
) -> Option<Vec<JourneyRouteSegment>> {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)?;
    let network = &state.region.rail_authority.rail_network;
    let direction = journey_service_direction(service, journey)?;
    let next_index = if direction > 0 {
        journey.current_stop_index.checked_add(1)?
    } else {
        journey.current_stop_index.checked_sub(1)?
    };
    let from_station_id = *service.stop_station_ids.get(journey.current_stop_index)?;
    let to_station_id = *service.stop_station_ids.get(next_index)?;
    let line_ids = path_between_stations(network, from_station_id, to_station_id).ok()?;

    let mut current_station_id = from_station_id;
    let mut segments = Vec::with_capacity(line_ids.len());
    for rail_line_id in line_ids {
        let line = network
            .rail_lines
            .iter()
            .find(|line| line.id == rail_line_id)?;
        let next_station_id = if line.first_station_id == current_station_id {
            line.second_station_id
        } else if line.second_station_id == current_station_id {
            line.first_station_id
        } else {
            return None;
        };
        segments.push(JourneyRouteSegment {
            rail_line_id: line.id,
            from_station_id: current_station_id,
            to_station_id: next_station_id,
            distance_metres: line.distance.metres(),
        });
        current_station_id = next_station_id;
    }

    (current_station_id == to_station_id).then_some(segments)
}

fn journey_service_direction(
    service: &crate::model::PassengerService,
    journey: &Journey,
) -> Option<i32> {
    let first = service.stop_station_ids.first().copied()?;
    let last = service.stop_station_ids.last().copied()?;
    if journey.origin_station_id == first && journey.destination_station_id == last {
        Some(1)
    } else if journey.origin_station_id == last && journey.destination_station_id == first {
        Some(-1)
    } else {
        None
    }
}

pub(super) fn journey_next_stop_station_id(
    state: &GameState,
    journey: &Journey,
) -> Option<RailStationId> {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)?;
    let direction = journey_service_direction(service, journey)?;
    let index = if direction > 0 {
        journey.current_stop_index.checked_add(1)?
    } else {
        journey.current_stop_index.checked_sub(1)?
    };
    service.stop_station_ids.get(index).copied()
}

pub(super) fn point_along_orthogonal_rail(
    start: (i32, i32),
    end: (i32, i32),
    progress: f64,
) -> ((i32, i32), char) {
    let horizontal_steps = (end.0 - start.0).unsigned_abs();
    let vertical_steps = (end.1 - start.1).unsigned_abs();
    let total_steps = horizontal_steps.saturating_add(vertical_steps);
    if total_steps == 0 {
        return (start, '▶');
    }

    let progress = progress.clamp(0.0, 1.0);
    let mut travelled_steps = if progress <= 0.0 {
        0
    } else if progress >= 1.0 {
        total_steps
    } else {
        ((progress * f64::from(total_steps)).floor() as u32)
            .max(1)
            .min(total_steps.saturating_sub(1).max(1))
    };

    if travelled_steps <= horizontal_steps && horizontal_steps > 0 {
        let direction = (end.0 - start.0).signum();
        let x = start.0.saturating_add(
            direction.saturating_mul(i32::try_from(travelled_steps).unwrap_or(i32::MAX)),
        );
        return ((x, start.1), if direction >= 0 { '▶' } else { '◀' });
    }

    travelled_steps = travelled_steps.saturating_sub(horizontal_steps);
    let direction = (end.1 - start.1).signum();
    let y = start.1.saturating_add(
        direction.saturating_mul(i32::try_from(travelled_steps).unwrap_or(i32::MAX)),
    );
    ((end.0, y), if direction >= 0 { '▼' } else { '▲' })
}

pub(super) fn ready_station_ids(state: &GameState) -> BTreeSet<RailStationId> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter_map(|train| match &train.status {
            TrainStatus::Ready { at } => Some(*at),
            TrainStatus::Travelling { .. } => None,
        })
        .collect()
}

pub(super) fn selected_neighbours(
    layout: &OperationalLayout,
    selected: Option<SettlementId>,
) -> BTreeSet<SettlementId> {
    let Some(selected) = selected else {
        return BTreeSet::new();
    };

    layout
        .lines
        .iter()
        .filter_map(|line| {
            if line.first_settlement_id == selected {
                Some(line.second_settlement_id)
            } else if line.second_settlement_id == selected {
                Some(line.first_settlement_id)
            } else {
                None
            }
        })
        .collect()
}

fn place_ink(
    place: &OperationalPlace,
    selected: Option<SettlementId>,
    adjacent: &BTreeSet<SettlementId>,
) -> MapInk {
    if selected == Some(place.settlement_id) {
        MapInk::Selected
    } else if adjacent.contains(&place.settlement_id) {
        MapInk::ConnectedAdjacent
    } else if place.station_id.is_some() {
        MapInk::Connected
    } else {
        MapInk::Unconnected
    }
}

pub(super) fn focus_rank(
    place: &OperationalPlace,
    selected: Option<SettlementId>,
    adjacent: &BTreeSet<SettlementId>,
) -> u8 {
    if selected == Some(place.settlement_id) {
        0
    } else if adjacent.contains(&place.settlement_id) {
        1
    } else if place.station_id.is_some() {
        2
    } else {
        3
    }
}

pub(super) fn map_place_label(place: &OperationalPlace, selected: Option<SettlementId>) -> String {
    if selected == Some(place.settlement_id) {
        place.name.to_uppercase()
    } else {
        place.name.clone()
    }
}

pub(super) fn place_link_distance_label(
    grid: &mut [Vec<MapCell>],
    start: (i32, i32),
    end: (i32, i32),
    distance_metres: u64,
) {
    let text = format_distance(distance_metres);
    let text_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let horizontal_steps = (end.0 - start.0).abs();
    let vertical_steps = (end.1 - start.1).abs();

    let candidates = if horizontal_steps >= vertical_steps && horizontal_steps > 0 {
        let midpoint_x = start.0 + (end.0 - start.0) / 2;
        let start_x = midpoint_x - text_width / 2;
        vec![(start_x, start.1 - 1), (start_x, start.1 + 1)]
    } else {
        let midpoint_y = start.1 + (end.1 - start.1) / 2;
        let rail_x = end.0;
        vec![
            (rail_x + 2, midpoint_y),
            (rail_x - text_width - 2, midpoint_y),
        ]
    };

    if let Some((x, y)) = candidates
        .into_iter()
        .find(|(x, y)| can_place_text(grid, *x, *y, &text))
    {
        put_text(grid, x, y, &text, MapInk::RailLabel);
    }
}

fn preferred_label_direction(
    place: &OperationalPlace,
    label: &str,
    places: &[OperationalPlace],
    positions: &BTreeMap<SettlementId, (i32, i32)>,
    selected: Option<SettlementId>,
) -> Option<MapDirection> {
    let &(x, y) = positions.get(&place.settlement_id)?;
    let own_width = i32::try_from(label.chars().count()).unwrap_or(i32::MAX);

    places
        .iter()
        .filter(|other| other.settlement_id != place.settlement_id)
        .filter_map(|other| {
            let &(other_x, other_y) = positions.get(&other.settlement_id)?;
            let other_label = map_place_label(other, selected);
            let other_width = i32::try_from(other_label.chars().count()).unwrap_or(i32::MAX);
            let dx = other_x - x;
            let dy = other_y - y;
            let horizontal = dx.abs() >= dy.abs();
            let crowded = if horizontal {
                dy.abs() <= 2 && dx.abs() <= ((own_width + other_width) / 2 + 6).max(12)
            } else {
                dx.abs() <= 3 && dy.abs() <= 6
            };
            crowded.then_some((dx.abs() + dy.abs(), dx, dy))
        })
        .min_by_key(|(distance, _, _)| *distance)
        .map(|(_, dx, dy)| {
            if dx.abs() >= dy.abs() {
                if dx > 0 {
                    MapDirection::Left
                } else {
                    MapDirection::Right
                }
            } else if dy > 0 {
                MapDirection::Up
            } else {
                MapDirection::Down
            }
        })
}

fn label_candidates(
    marker_x: i32,
    marker_y: i32,
    text: &str,
    preferred_direction: Option<MapDirection>,
) -> Vec<(i32, i32)> {
    let label_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let centered_x = marker_x - label_width / 2;
    let position_for = |direction| match direction {
        MapDirection::Up => (centered_x, marker_y - 1),
        MapDirection::Down => (centered_x, marker_y + 1),
        MapDirection::Right => (marker_x + 2, marker_y),
        MapDirection::Left => (marker_x - label_width - 2, marker_y),
    };

    let mut candidates = Vec::with_capacity(4);
    if let Some(direction) = preferred_direction {
        candidates.push(position_for(direction));
    }
    for direction in [
        MapDirection::Up,
        MapDirection::Down,
        MapDirection::Right,
        MapDirection::Left,
    ] {
        let candidate = position_for(direction);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn try_place_map_label(
    grid: &mut [Vec<MapCell>],
    marker_x: i32,
    marker_y: i32,
    text: &str,
    ink: MapInk,
    preferred_direction: Option<MapDirection>,
) -> bool {
    let Some((x, y)) = label_candidates(marker_x, marker_y, text, preferred_direction)
        .into_iter()
        .find(|(x, y)| can_place_text(grid, *x, *y, text))
    else {
        return false;
    };
    put_text(grid, x, y, text, ink);
    true
}

fn try_place_service_preview_label(
    grid: &mut [Vec<MapCell>],
    marker_x: i32,
    marker_y: i32,
    text: &str,
    ink: MapInk,
    preferred_direction: Option<MapDirection>,
) -> bool {
    let label_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let centered_x = marker_x - label_width / 2;
    let position_for = |direction, distance: i32| match direction {
        MapDirection::Up => (centered_x, marker_y - distance),
        MapDirection::Down => (centered_x, marker_y + distance),
        MapDirection::Right => (marker_x + distance + 1, marker_y),
        MapDirection::Left => (marker_x - label_width - distance - 1, marker_y),
    };

    let mut directions = Vec::with_capacity(4);
    if let Some(direction) = preferred_direction {
        directions.push(direction);
    }
    for direction in [
        MapDirection::Up,
        MapDirection::Down,
        MapDirection::Right,
        MapDirection::Left,
    ] {
        if !directions.contains(&direction) {
            directions.push(direction);
        }
    }

    // First keep labels close to their marker, then progressively fan them out
    // by a few cells. This is enough to separate neighbouring names such as
    // Oakridge/Fairford without turning the preview into floating annotations.
    for distance in 1..=3 {
        for direction in directions.iter().copied() {
            let (x, y) = position_for(direction, distance);
            if can_place_text(grid, x, y, text) {
                put_text(grid, x, y, text, ink);
                return true;
            }
        }
    }

    false
}

fn place_map_label(
    grid: &mut [Vec<MapCell>],
    marker_x: i32,
    marker_y: i32,
    text: &str,
    ink: MapInk,
    preferred_direction: Option<MapDirection>,
) {
    if try_place_map_label(grid, marker_x, marker_y, text, ink, preferred_direction) {
        return;
    }

    // Very small terminals may leave no collision-free row. Important labels
    // still get a deterministic fallback; lower-priority mini-map context uses
    // try_place_map_label directly and is simply omitted instead.
    let label_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    put_text(grid, marker_x - label_width / 2, marker_y - 1, text, ink);
}

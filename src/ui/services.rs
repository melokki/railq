//! Secondary Passenger Service workspace opened from the Map.
//!
//! This batch manages persistent directional stop patterns only. Manual Dispatch now selects from these persistent Services; intermediate-stop
//! boarding and alighting remain deferred to the next roadmap item.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::{
    model::{GameState, RailStationId, ServiceId},
    sim::services::service_path_for_stops,
};

use super::{format, theme};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceWorkspaceAction {
    Continue,
    Close,
    Create { stop_station_ids: Vec<RailStationId> },
    Delete { service_id: ServiceId },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceWorkspace {
    selected_service_index: usize,
    create_flow: Option<CreateServiceFlow>,
    delete_confirmation: Option<ServiceId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CreateServiceFlow {
    stop_station_ids: Vec<RailStationId>,
    selected_station_index: usize,
    review: bool,
    error: Option<String>,
}

impl ServiceWorkspace {
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) -> ServiceWorkspaceAction {
        if let Some(service_id) = self.delete_confirmation {
            return match key {
                KeyCode::Enter => ServiceWorkspaceAction::Delete { service_id },
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => {
                    self.delete_confirmation = None;
                    ServiceWorkspaceAction::Continue
                }
                _ => ServiceWorkspaceAction::Continue,
            };
        }

        if self.create_flow.is_some() {
            if key == KeyCode::Esc {
                self.create_flow = None;
                return ServiceWorkspaceAction::Continue;
            }

            let flow = self
                .create_flow
                .as_mut()
                .expect("the creation flow was checked above");
            if flow.review {
                return match key {
                    KeyCode::Enter => ServiceWorkspaceAction::Create {
                        stop_station_ids: flow.stop_station_ids.clone(),
                    },
                    KeyCode::Backspace | KeyCode::Left => {
                        flow.review = false;
                        flow.error = None;
                        ServiceWorkspaceAction::Continue
                    }
                    _ => ServiceWorkspaceAction::Continue,
                };
            }

            let station_count = state.region.rail_authority.rail_network.rail_stations.len();
            match key {
                KeyCode::Up | KeyCode::Char('k' | 'K') => {
                    flow.selected_station_index = flow.selected_station_index.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j' | 'J') => {
                    if station_count > 0 {
                        flow.selected_station_index = (flow.selected_station_index + 1)
                            .min(station_count.saturating_sub(1));
                    }
                }
                KeyCode::Backspace | KeyCode::Left => {
                    flow.stop_station_ids.pop();
                    flow.error = None;
                }
                KeyCode::Enter => {
                    let Some(station_id) = state
                        .region
                        .rail_authority
                        .rail_network
                        .rail_stations
                        .get(flow.selected_station_index)
                        .map(|station| station.id)
                    else {
                        return ServiceWorkspaceAction::Continue;
                    };
                    let mut candidate = flow.stop_station_ids.clone();
                    candidate.push(station_id);
                    if candidate.len() == 1 {
                        flow.stop_station_ids = candidate;
                        flow.error = None;
                    } else {
                        match service_path_for_stops(
                            &state.region.rail_authority.rail_network,
                            &candidate,
                        ) {
                            Ok(_) => {
                                flow.stop_station_ids = candidate;
                                flow.error = None;
                            }
                            Err(error) => flow.error = Some(error.to_string()),
                        }
                    }
                }
                KeyCode::Char('f' | 'F') => {
                    if flow.stop_station_ids.len() < 2 {
                        flow.error = Some("Add at least two stops before reviewing the Service.".into());
                    } else {
                        match service_path_for_stops(
                            &state.region.rail_authority.rail_network,
                            &flow.stop_station_ids,
                        ) {
                            Ok(_) => {
                                flow.review = true;
                                flow.error = None;
                            }
                            Err(error) => flow.error = Some(error.to_string()),
                        }
                    }
                }
                _ => {}
            }
            return ServiceWorkspaceAction::Continue;
        }

        match key {
            KeyCode::Esc => ServiceWorkspaceAction::Close,
            KeyCode::Char('n' | 'N') => {
                self.create_flow = Some(CreateServiceFlow::default());
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Char('d' | 'D') => {
                if let Some(service_id) = self.selected_service_id(state) {
                    self.delete_confirmation = Some(service_id);
                }
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k' | 'K') => {
                self.selected_service_index = self.selected_service_index.saturating_sub(1);
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j' | 'J') => {
                let len = state.player_company.passenger_services.len();
                if len > 0 {
                    self.selected_service_index =
                        (self.selected_service_index + 1).min(len.saturating_sub(1));
                }
                ServiceWorkspaceAction::Continue
            }
            _ => ServiceWorkspaceAction::Continue,
        }
    }

    pub fn confirm_created(&mut self, state: &GameState) {
        self.create_flow = None;
        self.delete_confirmation = None;
        self.selected_service_index = state
            .player_company
            .passenger_services
            .len()
            .saturating_sub(1);
    }

    pub fn confirm_deleted(&mut self, state: &GameState) {
        self.delete_confirmation = None;
        self.selected_service_index = self
            .selected_service_index
            .min(state.player_company.passenger_services.len().saturating_sub(1));
    }

    pub fn reject_action(&mut self, message: impl Into<String>) {
        if let Some(flow) = &mut self.create_flow {
            flow.review = false;
            flow.error = Some(message.into());
        }
    }

    /// Legacy textual description of the current Service controls.
    /// Kept for callers outside the shell; the in-game footer now renders
    /// structured shortcuts with per-key styling.
    pub fn controls(&self) -> &'static str {
        if self.delete_confirmation.is_some() {
            "Enter Delete  Esc Cancel"
        } else if let Some(flow) = &self.create_flow {
            if flow.review {
                "Enter Create  ←/Backspace Edit  Esc Cancel"
            } else {
                "↑↓/jk Station  Enter Add stop  Backspace Remove  f Review  Esc Cancel"
            }
        } else {
            "↑↓/jk Service  n New  d Delete  Esc Map"
        }
    }

    /// Contextual actions for the shared RailQ footer.  The footer owns the
    /// visual treatment so Passenger Services can describe behaviour without
    /// embedding presentation markup in a string.
    pub fn footer_shortcuts(&self, compact: bool) -> &'static [(&'static str, &'static str)] {
        if self.delete_confirmation.is_some() {
            &[("Enter", "Delete"), ("Esc", "Cancel")]
        } else if let Some(flow) = &self.create_flow {
            if flow.review {
                &[("Enter", "Create"), ("←", "Edit"), ("Esc", "Cancel")]
            } else if compact {
                &[
                    ("↑↓", "Station"),
                    ("Enter", "Add stop"),
                    ("Backspace", "Remove"),
                    ("F", "Review"),
                    ("Esc", "Cancel"),
                ]
            } else {
                &[
                    ("↑↓/JK", "Station"),
                    ("Enter", "Add stop"),
                    ("Backspace", "Remove"),
                    ("F", "Review"),
                    ("Esc", "Cancel"),
                ]
            }
        } else if compact {
            &[
                ("↑↓", "Service"),
                ("N", "New"),
                ("D", "Delete"),
                ("Esc", "Map"),
            ]
        } else {
            &[
                ("↑↓/JK", "Service"),
                ("N", "New"),
                ("D", "Delete"),
                ("Esc", "Map"),
            ]
        }
    }

    fn selected_service_id(&self, state: &GameState) -> Option<ServiceId> {
        state
            .player_company
            .passenger_services
            .get(self.selected_service_index)
            .map(|service| service.id)
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(flow) = &self.create_flow {
            render_create_flow(frame, area, state, flow);
            return;
        }
        render_service_list(
            frame,
            area,
            state,
            self.selected_service_index,
            self.delete_confirmation,
        );
    }
}

fn render_service_list(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
    delete_confirmation: Option<ServiceId>,
) {
    let [list_area, details_area] = Layout::horizontal([
        Constraint::Percentage(42),
        Constraint::Percentage(58),
    ])
    .areas(area);

    let services = &state.player_company.passenger_services;
    let lines = if services.is_empty() {
        vec![Line::from(Span::styled(
            "No Passenger Services yet. Press N to create one.",
            theme::secondary(),
        ))]
    } else {
        services
            .iter()
            .enumerate()
            .map(|(index, service)| {
                let marker = if index == selected_index { "> " } else { "  " };
                let route = route_label(state, &service.stop_station_ids);
                Line::from(vec![
                    Span::styled(marker, if index == selected_index { theme::focused_title() } else { theme::secondary() }),
                    Span::styled(format!("{}  ", service.name), theme::primary_value()),
                    Span::styled(route, theme::secondary()),
                ])
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Passenger Services"))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        list_area,
    );

    let detail_lines = services
        .get(selected_index)
        .map(|service| service_details(state, service.id, delete_confirmation))
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "Create a Service to define an ordered, directional stop pattern.",
                theme::secondary(),
            ))]
        });
    frame.render_widget(
        Paragraph::new(detail_lines)
            .block(panel("Service Details"))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        details_area,
    );
}

fn service_details(
    state: &GameState,
    service_id: ServiceId,
    delete_confirmation: Option<ServiceId>,
) -> Vec<Line<'static>> {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return Vec::new();
    };

    let distance = service
        .rail_line_ids
        .iter()
        .filter_map(|line_id| {
            state
                .region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .find(|line| line.id == *line_id)
                .map(|line| line.distance.metres())
        })
        .sum::<u64>();
    let active = state
        .active_journeys
        .iter()
        .filter(|journey| journey.service_id == service.id)
        .count();

    let mut lines = vec![
        Line::from(Span::styled(service.name.clone(), theme::focused_title())),
        Line::from(format!("Direction  {}", route_label(state, &service.stop_station_ids))),
        Line::from(format!("Distance   {}", format::distance(distance))),
        Line::from(format!("Stops      {}", service.stop_station_ids.len())),
        Line::from(format!("Active     {active} Journey(s)")),
        Line::from(""),
        Line::from(Span::styled("Ordered stops", theme::table_header())),
    ];
    lines.extend(service.stop_station_ids.iter().enumerate().map(|(index, station_id)| {
        Line::from(format!("{}. {}", index + 1, station_label(state, *station_id)))
    }));

    if delete_confirmation == Some(service.id) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            if active == 0 {
                "Delete this Service? Enter confirms; Esc cancels."
            } else {
                "This Service has an active Journey and cannot be deleted."
            },
            if active == 0 { theme::warning() } else { theme::error() },
        )));
    }
    lines
}

fn render_create_flow(frame: &mut Frame, area: Rect, state: &GameState, flow: &CreateServiceFlow) {
    let [stops_area, picker_area] = Layout::horizontal([
        Constraint::Percentage(42),
        Constraint::Percentage(58),
    ])
    .areas(area);

    let mut stop_lines = vec![Line::from(Span::styled(
        "Ordered stops",
        theme::table_header(),
    ))];
    if flow.stop_station_ids.is_empty() {
        stop_lines.push(Line::from(Span::styled(
            "No stops selected yet.",
            theme::secondary(),
        )));
    } else {
        stop_lines.extend(flow.stop_station_ids.iter().enumerate().map(|(index, station_id)| {
            Line::from(format!("{}. {}", index + 1, station_label(state, *station_id)))
        }));
    }
    frame.render_widget(
        Paragraph::new(stop_lines)
            .block(panel("New Passenger Service"))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        stops_area,
    );

    let lines = if flow.review {
        let distance = service_path_for_stops(
            &state.region.rail_authority.rail_network,
            &flow.stop_station_ids,
        )
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|line_id| {
            state
                .region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .find(|line| line.id == line_id)
                .map(|line| line.distance.metres())
        })
        .sum::<u64>();
        vec![
            Line::from(Span::styled("Review", theme::focused_title())),
            Line::from(""),
            Line::from(format!("Direction  {}", route_label(state, &flow.stop_station_ids))),
            Line::from(format!("Distance   {}", format::distance(distance))),
            Line::from(format!("Stops      {}", flow.stop_station_ids.len())),
            Line::from(""),
            Line::from(Span::styled(
                "Enter creates and saves this directional Service.",
                theme::success(),
            )),
        ]
    } else {
        let mut lines = vec![Line::from(Span::styled(
            "Choose the next stop",
            theme::table_header(),
        ))];
        for (index, station) in state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .enumerate()
        {
            let selected = index == flow.selected_station_index;
            lines.push(Line::from(vec![
                Span::styled(if selected { "> " } else { "  " }, if selected { theme::focused_title() } else { theme::secondary() }),
                Span::styled(station_label(state, station.id), if selected { theme::primary_value() } else { theme::secondary() }),
            ]));
        }
        if let Some(error) = &flow.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(error.clone(), theme::error())));
        }
        lines
    };

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(if flow.review { "Review Service" } else { "Add Stop" }))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        picker_area,
    );
}

fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(theme::THIN_BORDERS)
        .border_style(theme::border())
        .title(title)
        .title_style(theme::title())
        .style(theme::panel())
}

fn route_label(state: &GameState, stops: &[RailStationId]) -> String {
    stops
        .iter()
        .map(|station_id| station_label(state, *station_id))
        .collect::<Vec<_>>()
        .join(" → ")
}

fn station_label(state: &GameState, station_id: RailStationId) -> String {
    let network = &state.region.rail_authority.rail_network;
    let Some(station) = network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return format!("Station {}", station_id.get());
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| format!("Station {}", station_id.get()))
}

//! Secondary Passenger Service workspace opened from the Map.
//!
//! This batch manages persistent directional stop patterns only. Manual Dispatch now selects from these persistent Services; intermediate-stop
//! boarding and alighting remain deferred to the next roadmap item.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    catalog::model_for_train,
    model::{GameState, RailStationId, ServiceId},
    sim::{
        demand::effective_arrival_rate_per_hour,
        services::service_path_for_stops,
    },
};

use super::{format, modal, theme};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceWorkspaceAction {
    Continue,
    Close,
    Create {
        stop_station_ids: Vec<RailStationId>,
    },
    Update {
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
    },
    Delete {
        service_id: ServiceId,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceWorkspace {
    open: bool,
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
    editing_service_id: Option<ServiceId>,
}

impl ServiceWorkspace {
    /// Returns whether the Passenger Services workspace is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens Passenger Services without discarding any in-progress presentation state.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Returns to the Map while preserving any in-progress presentation state.
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) -> ServiceWorkspaceAction {
        if let Some(service_id) = self.delete_confirmation {
            return match key {
                KeyCode::Enter if service_active_journeys(state, service_id) == 0 => {
                    ServiceWorkspaceAction::Delete { service_id }
                }
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
                    KeyCode::Enter => match flow.editing_service_id {
                        Some(service_id) => ServiceWorkspaceAction::Update {
                            service_id,
                            stop_station_ids: flow.stop_station_ids.clone(),
                        },
                        None => ServiceWorkspaceAction::Create {
                            stop_station_ids: flow.stop_station_ids.clone(),
                        },
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
                        flow.selected_station_index =
                            (flow.selected_station_index + 1).min(station_count.saturating_sub(1));
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
                        flow.error =
                            Some("Add at least two stops before reviewing the Service.".into());
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
            KeyCode::Char('e' | 'E') => {
                if let Some(service_id) = self.selected_service_id(state) {
                    if service_active_journeys(state, service_id) == 0 {
                        if let Some(service) = state
                            .player_company
                            .passenger_services
                            .iter()
                            .find(|service| service.id == service_id)
                        {
                            let selected_station_index = service
                                .stop_station_ids
                                .last()
                                .and_then(|station_id| {
                                    state
                                        .region
                                        .rail_authority
                                        .rail_network
                                        .rail_stations
                                        .iter()
                                        .position(|station| station.id == *station_id)
                                })
                                .unwrap_or(0);
                            self.create_flow = Some(CreateServiceFlow {
                                stop_station_ids: service.stop_station_ids.clone(),
                                selected_station_index,
                                review: false,
                                error: None,
                                editing_service_id: Some(service_id),
                            });
                        }
                    }
                }
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Char('d' | 'D') => {
                if let Some(service_id) = self.selected_service_id(state) {
                    if service_active_journeys(state, service_id) == 0 {
                        self.delete_confirmation = Some(service_id);
                    }
                }
                ServiceWorkspaceAction::Continue
            }
            KeyCode::PageUp => {
                self.selected_service_index = self.selected_service_index.saturating_sub(8);
                ServiceWorkspaceAction::Continue
            }
            KeyCode::PageDown => {
                let len = state.player_company.passenger_services.len();
                if len > 0 {
                    self.selected_service_index =
                        (self.selected_service_index + 8).min(len.saturating_sub(1));
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
        self.open = true;
        self.create_flow = None;
        self.delete_confirmation = None;
        self.selected_service_index = state
            .player_company
            .passenger_services
            .len()
            .saturating_sub(1);
    }

    pub fn confirm_updated(&mut self, state: &GameState) {
        self.open = true;
        self.create_flow = None;
        self.delete_confirmation = None;
        self.selected_service_index = self.selected_service_index.min(
            state
                .player_company
                .passenger_services
                .len()
                .saturating_sub(1),
        );
    }

    pub fn confirm_deleted(&mut self, state: &GameState) {
        self.open = true;
        self.delete_confirmation = None;
        self.selected_service_index = self.selected_service_index.min(
            state
                .player_company
                .passenger_services
                .len()
                .saturating_sub(1),
        );
    }

    pub fn reject_action(&mut self, message: impl Into<String>) {
        if let Some(flow) = &mut self.create_flow {
            flow.review = false;
            flow.error = Some(message.into());
        }
    }

    /// Contextual actions for the shared RailQ footer.  The footer owns the
    /// visual treatment so Passenger Services can describe behaviour without
    /// embedding presentation markup in a string.
    pub fn footer_shortcuts(
        &self,
        compact: bool,
        wide: bool,
        state: &GameState,
    ) -> Vec<(&'static str, &'static str, bool)> {
        if let Some(service_id) = self.delete_confirmation {
            return if service_active_journeys(state, service_id) == 0 {
                vec![("Enter", "Delete", true), ("Esc", "Cancel", true)]
            } else {
                vec![("Esc", "Close", true)]
            };
        }

        if let Some(flow) = &self.create_flow {
            return if flow.review {
                vec![
                    (
                        "Enter",
                        if flow.editing_service_id.is_some() {
                            "Save"
                        } else {
                            "Create"
                        },
                        true,
                    ),
                    ("←", "Edit", true),
                    ("Esc", "Cancel", true),
                ]
            } else if compact {
                vec![
                    ("↑↓", "Station", true),
                    ("Enter", "Add stop", true),
                    ("Backspace", "Remove", true),
                    ("F", "Review", true),
                    ("Esc", "Cancel", true),
                ]
            } else {
                vec![
                    ("↑↓/JK", "Station", true),
                    ("Enter", "Add stop", true),
                    ("Backspace", "Remove", true),
                    ("F", "Review", true),
                    ("Esc", "Cancel", true),
                ]
            };
        }

        let has_services = !state.player_company.passenger_services.is_empty();
        let can_edit = self
            .selected_service_id(state)
            .map(|service_id| service_active_journeys(state, service_id) == 0)
            .unwrap_or(false);
        let can_delete = can_edit;
        let mut actions = vec![(
            if compact { "↑↓" } else { "↑↓/JK" },
            "Service",
            has_services,
        )];
        if wide && has_services {
            actions.push(("PgUp/PgDn", "Page", true));
        }
        actions.extend([
            ("N", "New", true),
            ("E", "Edit", can_edit),
            ("D", "Delete", can_delete),
            ("Esc", "Map", true),
        ]);
        actions
    }

    /// Contextual help for Passenger Services. Keeping this beside input and
    /// footer shortcuts means the workspace owns the interaction vocabulary.
    pub fn help_lines(&self, state: &GameState) -> Vec<String> {
        let mut lines = vec!["Current · Passenger Services".into()];
        if state.player_company.passenger_services.is_empty() {
            lines.extend([
                "n Create the first directional Passenger Service".into(),
                "Esc Return to Map".into(),
            ]);
        } else {
            lines.extend([
                "↑↓ / jk Select Passenger Service".into(),
                "PgUp / PgDn Move through longer Service lists".into(),
                "n Create a new directional Passenger Service".into(),
                "e Edit the selected Service when it has no active Journeys".into(),
                "d Delete the selected Service when it has no active Journeys".into(),
                "Esc Return to Map".into(),
            ]);
        }
        lines.extend([
            String::new(),
            "During create/edit: Enter adds a stop, Backspace removes the last stop, f reviews."
                .into(),
        ]);
        lines
    }

    fn selected_service_id(&self, state: &GameState) -> Option<ServiceId> {
        state
            .player_company
            .passenger_services
            .get(self.selected_service_index)
            .map(|service| service.id)
    }

    /// Returns whether Passenger Services currently owns a focused modal.
    pub fn has_modal(&self) -> bool {
        self.create_flow.is_some() || self.delete_confirmation.is_some()
    }

    /// Renders only the persistent Passenger Services workspace.  The shell
    /// draws focused modals in a later layer so it can dim the entire
    /// application underneath them, including the header and footer.
    pub fn render_base(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_service_list(frame, area, state, self.selected_service_index);
    }

    /// Renders the currently focused Passenger Services modal, if any.
    pub fn render_modal(&self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(flow) = &self.create_flow {
            render_create_flow(frame, modal::workflow_rect(area), state, flow);
        } else if let Some(service_id) = self.delete_confirmation {
            render_delete_confirmation(frame, area, state, service_id);
        }
    }

    /// Convenience renderer retained for callers outside the application
    /// shell.  The live shell uses `render_base` + `render_modal` as separate
    /// layers so the backdrop can be muted between them.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        self.render_base(frame, area, state);
        self.render_modal(frame, area, state);
    }
}

fn render_service_list(frame: &mut Frame, area: Rect, state: &GameState, selected_index: usize) {
    // Passenger Services is one workspace. The outer frame owns the view and
    // the list/detail split is expressed with quiet separators rather than
    // competing bordered panels.
    let shell = panel("Passenger Services");
    let inner = shell.inner(area);
    frame.render_widget(shell, area);
    let content = workspace_inset(inner, 1);

    let services = &state.player_company.passenger_services;
    if services.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("No Passenger Services yet", theme::title()),
                Line::from(""),
                Line::styled(
                    "Services are reusable directional routes used by Manual Dispatch.",
                    theme::secondary(),
                ),
                Line::styled(
                    "Define an ordered stop pattern across connected Rail Stations.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            content,
        );
        return;
    }

    let selected_index = selected_index.min(services.len().saturating_sub(1));
    if content.width >= 96 && content.height >= 18 {
        render_wide_service_workspace(frame, content, state, selected_index);
    } else {
        render_compact_service_workspace(frame, content, state, selected_index);
    }
}

fn render_wide_service_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
) {
    let [list_area, divider_area, inspector_area] = Layout::horizontal([
        Constraint::Min(40),
        Constraint::Length(1),
        Constraint::Length(46),
    ])
    .areas(area);

    render_service_picker(frame, list_area, state, selected_index, true);
    modal::render_vertical_separator(frame, divider_area);
    render_service_inspector(
        frame,
        workspace_inset(inspector_area, 1),
        state,
        selected_index,
    );
}

fn render_compact_service_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
) {
    let list_height = if area.height < 20 {
        area.height / 3
    } else {
        area.height.saturating_mul(2) / 5
    };
    let [list_area, divider_area, inspector_area] = Layout::vertical([
        Constraint::Length(list_height.max(5)),
        Constraint::Length(1),
        Constraint::Min(5),
    ])
    .areas(area);

    render_service_picker(frame, list_area, state, selected_index, false);
    modal::render_horizontal_separator(frame, divider_area);
    render_service_inspector(frame, inspector_area, state, selected_index);
}

fn render_service_picker(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
    wide: bool,
) {
    let services = &state.player_company.passenger_services;
    let rows = services
        .iter()
        .map(|service| {
            let direction = service_direction_label(state, service);
            if wide {
                let snapshot = service_operating_snapshot(state, service.id);
                let (state_label, state_style) = if snapshot.active_trains > 0 {
                    ("LIVE", theme::success())
                } else {
                    ("IDLE", theme::secondary())
                };
                Row::new(vec![
                    Cell::from(service.name.clone()),
                    Cell::from(direction),
                    Cell::from(state_label).style(state_style),
                    Cell::from(snapshot.active_trains.to_string()),
                    Cell::from(snapshot.waiting_passengers.to_string()),
                ])
            } else {
                Row::new([service.name.clone(), direction])
            }
        })
        .collect::<Vec<_>>();

    let (header, widths) = if wide {
        (
            Row::new(["Service", "Direction", "State", "Trains", "Waiting"])
                .style(theme::table_header())
                .bottom_margin(1),
            vec![
                Constraint::Length(10),
                Constraint::Min(16),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(8),
            ],
        )
    } else {
        (
            Row::new(["Service", "Direction"])
                .style(theme::table_header())
                .bottom_margin(1),
            vec![Constraint::Length(10), Constraint::Min(16)],
        )
    };
    let table = Table::new(rows, widths)
        .header(header)
        .style(theme::panel())
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    let mut table_state = TableState::default();
    table_state.select(Some(selected_index));
    frame.render_stateful_widget(table, area, &mut table_state);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceInspectorDensity {
    Full,
    Compact,
    Tight,
}

fn render_service_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
) {
    // Short terminals need a genuinely smaller information set, not just the
    // full inspector with blank lines removed.  Keeping three density levels
    // prevents the operational summary from being clipped at 80×24.
    let density = if area.height < 22 || area.width < 34 {
        ServiceInspectorDensity::Tight
    } else if area.height < 30 || area.width < 42 {
        ServiceInspectorDensity::Compact
    } else {
        ServiceInspectorDensity::Full
    };
    let lines = state
        .player_company
        .passenger_services
        .get(selected_index)
        .map(|service| service_details(state, service.id, density, area.width as usize))
        .unwrap_or_else(|| {
            vec![Line::styled(
                "No Passenger Service selected.",
                theme::secondary(),
            )]
        });
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn workspace_inset(area: Rect, horizontal: u16) -> Rect {
    let padding = horizontal.min(area.width / 2);
    Rect {
        x: area.x.saturating_add(padding),
        y: area.y,
        width: area.width.saturating_sub(padding.saturating_mul(2)),
        height: area.height,
    }
}

fn service_details(
    state: &GameState,
    service_id: ServiceId,
    density: ServiceInspectorDensity,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return Vec::new();
    };

    let dense = density != ServiceInspectorDensity::Full;
    let tight = density == ServiceInspectorDensity::Tight;
    let value_width = width.saturating_sub(16).max(6);

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
    let snapshot = service_operating_snapshot(state, service_id);
    let state_label = if snapshot.active_trains > 0 {
        "IN SERVICE"
    } else {
        "IDLE"
    };
    let state_style = if snapshot.active_trains > 0 {
        theme::success()
    } else {
        theme::secondary()
    };
    let origin = service
        .origin_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let destination = service
        .destination_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());

    let mut lines = vec![Line::styled(service.name.clone(), theme::focused_title())];

    if tight {
        lines.push(Line::styled(state_label.to_owned(), state_style));
    } else {
        inspector_section(&mut lines, "STATUS", dense);
        lines.push(labelled_line_styled("State", state_label, state_style));
    }

    inspector_section(&mut lines, "ROUTE", dense);
    lines.push(labelled_line(
        "Direction",
        &truncate_display(&format!("{origin} → {destination}"), value_width),
    ));
    if !tight {
        lines.push(labelled_line("Distance", &format::distance(distance)));
        lines.push(labelled_line(
            "Stops",
            &service.stop_station_ids.len().to_string(),
        ));
    }

    inspector_section(&mut lines, "OPERATIONS", dense);
    if !tight {
        lines.push(labelled_line(
            "Active trains",
            &snapshot.active_trains.to_string(),
        ));
    }
    let next_arrival = snapshot
        .running_trains
        .first()
        .map(|train| {
            format!(
                "{} · {} · in {}",
                train.label,
                station_label(state, train.next_station_id),
                format::duration(train.remaining_seconds)
            )
        })
        .unwrap_or_else(|| "—".into());
    lines.push(labelled_line(
        "Next arrival",
        &truncate_display(&next_arrival, value_width),
    ));

    if snapshot.active_trains > 0 && !tight {
        inspector_section(&mut lines, "RUNNING TRAINS", dense);
        let visible_trains = if dense { 1 } else { 2 };
        for train in snapshot.running_trains.iter().take(visible_trains) {
            lines.push(Line::styled(
                truncate_display(&format!("{}  {}", train.label, train.current_leg), width),
                theme::primary_value(),
            ));
            let load = if train.capacity > 0 {
                let percent = u64::from(train.onboard_passengers).saturating_mul(100)
                    / u64::from(train.capacity);
                format!(
                    "ETA {} · Load {}/{} · {}%",
                    format::duration(train.remaining_seconds),
                    train.onboard_passengers,
                    train.capacity,
                    percent
                )
            } else {
                format!(
                    "ETA {} · Load {}",
                    format::duration(train.remaining_seconds),
                    train.onboard_passengers
                )
            };
            lines.push(Line::styled(format!("  {load}"), theme::secondary()));
        }
        if snapshot.running_trains.len() > visible_trains {
            lines.push(Line::styled(
                format!(
                    "… +{} more running",
                    snapshot.running_trains.len() - visible_trains
                ),
                theme::secondary(),
            ));
        }
    }

    inspector_section(&mut lines, "PASSENGERS", dense);
    let waiting = format!(
        "{} · +{}/h",
        snapshot.waiting_passengers, snapshot.arrival_rate_per_hour
    );
    if !tight || snapshot.active_trains == 0 {
        lines.push(labelled_line(
            "Waiting",
            &truncate_display(&waiting, value_width),
        ));
    }
    if snapshot.active_trains > 0 || !dense {
        let onboard = if snapshot.total_capacity > 0 {
            let percent = u64::from(snapshot.onboard_passengers).saturating_mul(100)
                / u64::from(snapshot.total_capacity);
            format!(
                "{} / {} · {}%",
                snapshot.onboard_passengers, snapshot.total_capacity, percent
            )
        } else {
            snapshot.onboard_passengers.to_string()
        };
        lines.push(labelled_line("On board", &onboard));
        if !tight {
            lines.push(labelled_line(
                "Carried",
                &snapshot.passengers_carried.to_string(),
            ));
        }
    }

    if snapshot.active_trains > 0 {
        inspector_section(&mut lines, "COMMERCIAL", dense);
        if !tight {
            lines.push(labelled_line(
                "Expected revenue",
                &format_cents(snapshot.booked_revenue_cents),
            ));
            if !dense {
                lines.push(labelled_line(
                    "Credited",
                    &format_cents(snapshot.credited_revenue_cents),
                ));
            }
            lines.push(labelled_line(
                "Operating cost",
                &format_cents(snapshot.operating_cost_cents),
            ));
        }
        let result = snapshot
            .booked_revenue_cents
            .saturating_sub(snapshot.operating_cost_cents);
        let result_style = if result >= 0 {
            theme::success()
        } else {
            theme::warning()
        };
        lines.push(labelled_line_styled(
            "Expected result",
            &format::signed_cents(result),
            result_style,
        ));
    }

    if density == ServiceInspectorDensity::Full && snapshot.active_trains == 0 {
        inspector_section(&mut lines, "STOP PATTERN", false);
        const MAX_VISIBLE_STOPS: usize = 6;
        lines.extend(
            service
                .stop_station_ids
                .iter()
                .take(MAX_VISIBLE_STOPS)
                .enumerate()
                .map(|(index, station_id)| {
                    Line::from(format!(
                        "{}. {}",
                        index + 1,
                        station_label(state, *station_id)
                    ))
                }),
        );
        if service.stop_station_ids.len() > MAX_VISIBLE_STOPS {
            lines.push(Line::styled(
                format!(
                    "… +{} more stops",
                    service.stop_station_ids.len() - MAX_VISIBLE_STOPS
                ),
                theme::secondary(),
            ));
        }
    }

    lines
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunningTrainSnapshot {
    label: String,
    current_leg: String,
    next_station_id: RailStationId,
    remaining_seconds: u64,
    onboard_passengers: u32,
    capacity: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ServiceOperatingSnapshot {
    active_trains: usize,
    waiting_passengers: u32,
    arrival_rate_per_hour: u32,
    onboard_passengers: u32,
    total_capacity: u32,
    passengers_carried: u32,
    booked_revenue_cents: i128,
    credited_revenue_cents: i128,
    operating_cost_cents: i128,
    running_trains: Vec<RunningTrainSnapshot>,
}

fn service_operating_snapshot(
    state: &GameState,
    service_id: ServiceId,
) -> ServiceOperatingSnapshot {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return ServiceOperatingSnapshot::default();
    };

    let mut snapshot = ServiceOperatingSnapshot::default();
    let (waiting, arrival_rate) = service_waiting_demand(state, &service.stop_station_ids);
    snapshot.waiting_passengers = waiting;
    snapshot.arrival_rate_per_hour = arrival_rate;

    for journey in state
        .active_journeys
        .iter()
        .filter(|journey| journey.service_id == service_id)
    {
        snapshot.active_trains = snapshot.active_trains.saturating_add(1);
        snapshot.onboard_passengers = snapshot
            .onboard_passengers
            .saturating_add(journey.onboard_passengers());
        snapshot.passengers_carried = snapshot
            .passengers_carried
            .saturating_add(journey.passengers_carried);
        snapshot.booked_revenue_cents = snapshot
            .booked_revenue_cents
            .saturating_add(i128::from(journey.operating_revenue.cents()));
        snapshot.credited_revenue_cents = snapshot
            .credited_revenue_cents
            .saturating_add(i128::from(journey.credited_revenue.cents()));
        snapshot.operating_cost_cents = snapshot
            .operating_cost_cents
            .saturating_add(i128::from(journey.infrastructure_access_fee.cents()))
            .saturating_add(i128::from(journey.fuel_cost.cents()));

        let remaining = remaining_journey_seconds(state, journey.arrives_at);
        let current_station_id = service
            .stop_station_ids
            .get(journey.current_stop_index)
            .copied()
            .unwrap_or(journey.origin_station_id);
        let next_station_id = service
            .stop_station_ids
            .get(journey.current_stop_index.saturating_add(1))
            .copied()
            .unwrap_or(journey.destination_station_id);

        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == journey.train_id);
        let capacity = train
            .and_then(model_for_train)
            .map(|model| model.passenger_capacity().passengers())
            .unwrap_or(0);
        snapshot.total_capacity = snapshot.total_capacity.saturating_add(capacity);
        let label = train
            .and_then(|train| {
                train
                    .nickname
                    .as_ref()
                    .map(|nickname| nickname.as_str().to_owned())
            })
            .unwrap_or_else(|| format!("Train {:02}", journey.train_id.get()));

        snapshot.running_trains.push(RunningTrainSnapshot {
            label,
            current_leg: format!(
                "{} → {}",
                station_label(state, current_station_id),
                station_label(state, next_station_id)
            ),
            next_station_id,
            remaining_seconds: remaining,
            onboard_passengers: journey.onboard_passengers(),
            capacity,
        });
    }

    snapshot
        .running_trains
        .sort_by_key(|train| train.remaining_seconds);
    snapshot
}

fn service_waiting_demand(state: &GameState, stops: &[RailStationId]) -> (u32, u32) {
    let mut waiting = 0_u32;
    let mut arrival_rate = 0_u32;
    for (origin_index, origin_station_id) in stops.iter().enumerate() {
        for destination_station_id in stops.iter().skip(origin_index.saturating_add(1)) {
            if let Some(demand) = state.origin_destination_demand.iter().find(|demand| {
                demand.origin_station_id == *origin_station_id
                    && demand.destination_station_id == *destination_station_id
            }) {
                waiting = waiting.saturating_add(demand.waiting_passengers);
                arrival_rate = arrival_rate
                    .saturating_add(effective_arrival_rate_per_hour(state, demand));
            }
        }
    }
    (waiting, arrival_rate)
}

fn remaining_journey_seconds(state: &GameState, arrives_at: crate::model::UtcSeconds) -> u64 {
    let remaining = arrives_at
        .unix_seconds()
        .saturating_sub(state.last_processed_at.unix_seconds());
    u64::try_from(remaining).unwrap_or(0)
}

fn format_cents(cents: i128) -> String {
    let formatted = format::signed_cents(cents);
    formatted.strip_prefix('+').unwrap_or(&formatted).to_owned()
}

fn inspector_section(lines: &mut Vec<Line<'static>>, title: &str, dense: bool) {
    if !dense {
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(title.to_owned(), theme::table_header()));
}

fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
}

fn labelled_line_styled(label: &str, value: &str, style: ratatui::style::Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), theme::secondary()),
        Span::styled(value.to_owned(), style),
    ])
}

fn service_active_journeys(state: &GameState, service_id: ServiceId) -> usize {
    state
        .active_journeys
        .iter()
        .filter(|journey| journey.service_id == service_id)
        .count()
}

fn render_delete_confirmation(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    service_id: ServiceId,
) {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return;
    };
    let active = service_active_journeys(state, service_id);
    let card = modal::centered_rect(area, 64, 14);
    let footer = if active == 0 {
        modal::shortcut_line(&[("Enter", "delete"), ("Esc", "cancel")])
    } else {
        modal::shortcut_line(&[("Esc", "close")])
    };
    let modal_areas = modal::render_shell(frame, card, "Delete Passenger Service", footer);

    let mut lines = vec![
        Line::styled(service.name.clone(), theme::focused_title()),
        Line::from(route_label(state, &service.stop_station_ids)),
        Line::from(""),
    ];
    if active == 0 {
        lines.extend([
            Line::styled("Delete this Service?", theme::warning()),
            Line::from("The saved stop pattern will be removed."),
            Line::from(Span::styled(
                "This does not sell Trains or change completed Journey receipts.",
                theme::secondary(),
            )),
        ]);
    } else {
        lines.extend([
            Line::styled("Deletion unavailable", theme::error()),
            Line::from(format!(
                "{active} active Journey(s) still use this Service."
            )),
            Line::from(Span::styled(
                "Wait for those Journeys to arrive before deleting it.",
                theme::secondary(),
            )),
        ]);
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

fn render_create_flow(frame: &mut Frame, area: Rect, state: &GameState, flow: &CreateServiceFlow) {
    let title = flow
        .editing_service_id
        .and_then(|service_id| {
            state
                .player_company
                .passenger_services
                .iter()
                .find(|service| service.id == service_id)
                .map(|service| format!("Edit Passenger Service · {}", service.name))
        })
        .unwrap_or_else(|| "Create Passenger Service".to_owned());
    let modal_areas = modal::render_shell(
        frame,
        area,
        &title,
        create_service_footer_line(flow, area.width),
    );

    if flow.review {
        render_create_service_review(frame, modal_areas.body, state, flow);
    } else {
        render_create_service_picker(frame, modal_areas.body, state, flow);
    }
}

fn create_service_footer_line(flow: &CreateServiceFlow, width: u16) -> Line<'static> {
    if flow.review {
        return modal::shortcut_line(&[
            (
                "Enter",
                if flow.editing_service_id.is_some() {
                    "save"
                } else {
                    "create"
                },
            ),
            ("←", "edit"),
            ("Esc", "cancel"),
        ]);
    }

    if width >= 76 {
        modal::shortcut_line(&[
            ("↑/↓", "choose"),
            ("Enter", "add"),
            ("Backspace", "remove"),
            ("F", "review"),
            ("Esc", "cancel"),
        ])
    } else {
        // On narrow terminals keep the primary progression controls readable;
        // navigation and undo remain available and are also exposed globally.
        modal::shortcut_line(&[("Enter", "add"), ("F", "review"), ("Esc", "cancel")])
    }
}

fn render_create_service_picker(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &CreateServiceFlow,
) {
    let status_rows = if flow.error.is_some() { 2 } else { 0 };
    let [context_area, content_area, status_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(status_rows),
    ])
    .areas(area);

    let route_summary = if flow.stop_station_ids.is_empty() {
        "No stops selected yet".to_owned()
    } else {
        route_label(state, &flow.stop_station_ids)
    };
    frame.render_widget(
        Paragraph::new(vec![
            create_service_step_line(1),
            Line::from(vec![
                Span::styled("ROUTE  ", theme::secondary()),
                Span::styled(
                    truncate_display(
                        &route_summary,
                        context_area.width.saturating_sub(7) as usize,
                    ),
                    theme::primary_value(),
                ),
            ]),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    let show_preview = content_area.width >= 68 && content_area.height >= 7;
    let (picker_area, divider_area, preview_area) = if show_preview {
        let [picker_area, divider_area, preview_area] = Layout::horizontal([
            Constraint::Min(30),
            Constraint::Length(1),
            Constraint::Length(32),
        ])
        .areas(content_area);
        (picker_area, Some(divider_area), Some(preview_area))
    } else {
        (content_area, None, None)
    };

    let [picker_title_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(picker_area);
    frame.render_widget(
        Paragraph::new(Line::styled("Choose the next stop", theme::title())).style(theme::panel()),
        picker_title_area,
    );

    let rows = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| Row::new([station_label(state, station.id)]))
        .collect::<Vec<_>>();
    let mut table_state = TableState::default();
    table_state.select(
        (!rows.is_empty()).then_some(
            flow.selected_station_index
                .min(rows.len().saturating_sub(1)),
        ),
    );
    let table = Table::new(rows, [Constraint::Min(1)])
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut table_state);

    if let (Some(divider_area), Some(preview_area)) = (divider_area, preview_area) {
        modal::render_vertical_separator(frame, divider_area);
        render_create_service_preview(frame, preview_area, state, flow);
    }

    if let Some(error) = &flow.error {
        frame.render_widget(
            Paragraph::new(Line::styled(error.clone(), theme::error()))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
}

fn render_create_service_preview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &CreateServiceFlow,
) {
    let selected_station_id = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .get(flow.selected_station_index)
        .map(|station| station.id);

    let mut lines = vec![Line::styled("Route Preview", theme::title())];
    if flow.stop_station_ids.is_empty() {
        lines.push(Line::styled(
            "The first stop becomes the Service origin.",
            theme::secondary(),
        ));
    } else {
        lines.push(Line::styled("ORDERED STOPS", theme::table_header()));
        let max_stop_rows = area.height.saturating_sub(8).max(1) as usize;
        lines.extend(preview_stop_lines(
            state,
            &flow.stop_station_ids,
            max_stop_rows,
        ));
    }

    lines.push(Line::from(""));
    lines.push(Line::styled("SELECTED", theme::table_header()));
    let Some(station_id) = selected_station_id else {
        lines.push(Line::styled(
            "No Rail Station available.",
            theme::secondary(),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    lines.push(Line::styled(
        station_label(state, station_id),
        theme::focused_title(),
    ));

    if flow.stop_station_ids.is_empty() {
        lines.push(Line::styled("Valid origin stop", theme::success()));
    } else {
        let mut candidate = flow.stop_station_ids.clone();
        candidate.push(station_id);
        match service_path_for_stops(&state.region.rail_authority.rail_network, &candidate) {
            Ok(line_ids) => {
                let distance = distance_for_line_ids(state, &line_ids);
                lines.push(Line::styled("Valid next stop", theme::success()));
                lines.push(Line::styled(
                    format!("Route after add · {}", format::distance(distance)),
                    theme::secondary(),
                ));
            }
            Err(error) => lines.push(Line::styled(error.to_string(), theme::error())),
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_create_service_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &CreateServiceFlow,
) {
    let line_ids = service_path_for_stops(
        &state.region.rail_authority.rail_network,
        &flow.stop_station_ids,
    )
    .unwrap_or_default();
    let distance = distance_for_line_ids(state, &line_ids);

    let [context_area, summary_area, stops_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(4),
        Constraint::Min(3),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(vec![
            create_service_step_line(2),
            Line::styled(
                if flow.editing_service_id.is_some() {
                    "Review Service Changes"
                } else {
                    "Review Passenger Service"
                },
                theme::title(),
            ),
        ])
        .style(theme::panel()),
        context_area,
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("DIRECTION  ", theme::secondary()),
                Span::styled(
                    truncate_display(
                        &stop_direction_label(state, &flow.stop_station_ids),
                        summary_area.width.saturating_sub(11) as usize,
                    ),
                    theme::focused_title(),
                ),
            ]),
            Line::from(vec![
                Span::styled("DISTANCE   ", theme::secondary()),
                Span::styled(format::distance(distance), theme::primary_value()),
            ]),
            Line::from(vec![
                Span::styled("STOPS      ", theme::secondary()),
                Span::styled(
                    flow.stop_station_ids.len().to_string(),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(
                if flow.editing_service_id.is_some() {
                    "Service identity is preserved; the ordered stop pattern will be replaced."
                } else {
                    "Directional Service · dispatchable from its first stop."
                },
                theme::secondary(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        summary_area,
    );

    let stop_lines = review_stop_lines(state, &flow.stop_station_ids, stops_area.height as usize);
    frame.render_widget(
        Paragraph::new(stop_lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        stops_area,
    );
}

fn service_direction_label(state: &GameState, service: &crate::model::PassengerService) -> String {
    stop_direction_label(state, &service.stop_station_ids)
}

fn stop_direction_label(state: &GameState, stop_station_ids: &[RailStationId]) -> String {
    let origin = stop_station_ids
        .first()
        .copied()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let destination = stop_station_ids
        .last()
        .copied()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    format!("{origin} → {destination}")
}

fn preview_stop_lines(
    state: &GameState,
    stop_station_ids: &[RailStationId],
    max_rows: usize,
) -> Vec<Line<'static>> {
    if stop_station_ids.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    if stop_station_ids.len() <= max_rows {
        return stop_station_ids
            .iter()
            .enumerate()
            .map(|(index, station_id)| {
                Line::from(vec![
                    Span::styled(format!("{}  ", index + 1), theme::secondary()),
                    Span::styled(station_label(state, *station_id), theme::primary_value()),
                ])
            })
            .collect();
    }

    if max_rows == 1 {
        return vec![Line::styled(
            format!("… {} stops selected", stop_station_ids.len()),
            theme::secondary(),
        )];
    }

    let visible_tail = max_rows.saturating_sub(1);
    let hidden = stop_station_ids.len().saturating_sub(visible_tail);
    let mut lines = vec![Line::styled(
        format!("… +{hidden} earlier stops"),
        theme::secondary(),
    )];
    lines.extend(
        stop_station_ids
            .iter()
            .enumerate()
            .skip(hidden)
            .map(|(index, station_id)| {
                Line::from(vec![
                    Span::styled(format!("{}  ", index + 1), theme::secondary()),
                    Span::styled(station_label(state, *station_id), theme::primary_value()),
                ])
            }),
    );
    lines
}

fn review_stop_lines(
    state: &GameState,
    stop_station_ids: &[RailStationId],
    max_lines: usize,
) -> Vec<Line<'static>> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut lines = vec![Line::styled("ORDERED STOPS", theme::table_header())];
    let row_capacity = max_lines.saturating_sub(1);
    if row_capacity == 0 || stop_station_ids.is_empty() {
        return lines;
    }

    let stop_line = |index: usize, station_id: RailStationId| {
        let suffix = if index == 0 {
            "  origin"
        } else if index + 1 == stop_station_ids.len() {
            "  destination"
        } else {
            ""
        };
        Line::from(vec![
            Span::styled(format!("{}  ", index + 1), theme::secondary()),
            Span::styled(station_label(state, station_id), theme::primary_value()),
            Span::styled(suffix, theme::secondary()),
        ])
    };

    if stop_station_ids.len() <= row_capacity {
        lines.extend(
            stop_station_ids
                .iter()
                .copied()
                .enumerate()
                .map(|(index, station_id)| stop_line(index, station_id)),
        );
        return lines;
    }

    if row_capacity == 1 {
        lines.push(Line::styled(
            format!("{} stops selected", stop_station_ids.len()),
            theme::secondary(),
        ));
        return lines;
    }

    lines.push(stop_line(0, stop_station_ids[0]));
    if row_capacity == 2 {
        let last = stop_station_ids.len() - 1;
        lines.push(stop_line(last, stop_station_ids[last]));
        return lines;
    }

    let middle_slots = row_capacity.saturating_sub(3);
    for (index, station_id) in stop_station_ids
        .iter()
        .copied()
        .enumerate()
        .skip(1)
        .take(middle_slots)
    {
        lines.push(stop_line(index, station_id));
    }
    let hidden_middle = stop_station_ids
        .len()
        .saturating_sub(2)
        .saturating_sub(middle_slots);
    if hidden_middle > 0 {
        lines.push(Line::styled(
            format!("… +{hidden_middle} intermediate stops"),
            theme::secondary(),
        ));
    }
    let last = stop_station_ids.len() - 1;
    lines.push(stop_line(last, stop_station_ids[last]));
    lines.truncate(max_lines);
    lines
}

fn truncate_display(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_owned();
    }

    let target = max_width - 1;
    let mut width = 0usize;
    let mut shortened = String::new();
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > target {
            break;
        }
        shortened.push(character);
        width = width.saturating_add(character_width);
    }
    shortened.push('…');
    shortened
}

fn create_service_step_line(active: u8) -> Line<'static> {
    let mut spans = Vec::new();
    for (step, label) in [(1, "STOPS"), (2, "REVIEW")] {
        if step > 1 {
            spans.push(Span::styled("  →  ", theme::secondary()));
        }
        let style = if step == active {
            theme::focused_title()
        } else if step < active {
            theme::success()
        } else {
            theme::secondary()
        };
        spans.push(Span::styled(format!("{step} {label}"), style));
    }
    Line::from(spans)
}

fn distance_for_line_ids(state: &GameState, line_ids: &[crate::model::RailLineId]) -> u64 {
    line_ids
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
        .sum()
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

//! Secondary Passenger Service workspace opened from the Map.
//!
//! This batch manages persistent directional stop patterns only. Manual Dispatch now selects from these persistent Services; intermediate-stop
//! boarding and alighting remain deferred to the next roadmap item.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{GameState, RailStationId, ServiceId},
    sim::services::service_path_for_stops,
};

use super::{format, modal, theme};

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
    pub fn footer_shortcuts(
        &self,
        compact: bool,
        state: &GameState,
    ) -> &'static [(&'static str, &'static str)] {
        if let Some(service_id) = self.delete_confirmation {
            if service_active_journeys(state, service_id) == 0 {
                &[("Enter", "Delete"), ("Esc", "Cancel")]
            } else {
                &[("Esc", "Close")]
            }
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
        // Keep the workspace visible behind focused workflows. This mirrors the
        // modal grammar used by Manual Dispatch and makes Create Service feel
        // like a temporary task rather than an entirely different screen.
        render_service_list(frame, area, state, self.selected_service_index);

        if let Some(flow) = &self.create_flow {
            render_create_flow(frame, create_service_modal_rect(area), state, flow);
        } else if let Some(service_id) = self.delete_confirmation {
            render_delete_confirmation(frame, area, state, service_id);
        }
    }
}

fn render_service_list(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_index: usize,
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
        .map(|service| service_details(state, service.id))
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

    lines
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
    let modal_areas = modal::render_shell(
        frame,
        area,
        "Create Passenger Service",
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
            ("Enter", "create"),
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
        modal::shortcut_line(&[
            ("Enter", "add"),
            ("F", "review"),
            ("Esc", "cancel"),
        ])
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
                Span::styled(route_summary, theme::primary_value()),
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
        Paragraph::new(Line::styled("Choose the next stop", theme::title()))
            .style(theme::panel()),
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
        (!rows.is_empty()).then_some(flow.selected_station_index.min(rows.len().saturating_sub(1))),
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
        lines.extend(
            flow.stop_station_ids
                .iter()
                .enumerate()
                .map(|(index, station_id)| {
                    Line::from(vec![
                        Span::styled(format!("{}  ", index + 1), theme::secondary()),
                        Span::styled(station_label(state, *station_id), theme::primary_value()),
                    ])
                }),
        );
    }

    lines.push(Line::from(""));
    lines.push(Line::styled("SELECTED", theme::table_header()));
    let Some(station_id) = selected_station_id else {
        lines.push(Line::styled("No Rail Station available.", theme::secondary()));
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
            Line::styled("Review Passenger Service", theme::title()),
        ])
        .style(theme::panel()),
        context_area,
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("DIRECTION  ", theme::secondary()),
                Span::styled(route_label(state, &flow.stop_station_ids), theme::focused_title()),
            ]),
            Line::from(vec![
                Span::styled("DISTANCE   ", theme::secondary()),
                Span::styled(format::distance(distance), theme::primary_value()),
            ]),
            Line::from(vec![
                Span::styled("STOPS      ", theme::secondary()),
                Span::styled(flow.stop_station_ids.len().to_string(), theme::primary_value()),
            ]),
            Line::styled(
                "Directional Service · dispatchable from its first stop.",
                theme::secondary(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        summary_area,
    );

    let mut stop_lines = vec![Line::styled("ORDERED STOPS", theme::table_header())];
    stop_lines.extend(
        flow.stop_station_ids
            .iter()
            .enumerate()
            .map(|(index, station_id)| {
                let is_origin = index == 0;
                let is_destination = index + 1 == flow.stop_station_ids.len();
                let suffix = if is_origin {
                    "  origin"
                } else if is_destination {
                    "  destination"
                } else {
                    ""
                };
                Line::from(vec![
                    Span::styled(format!("{}  ", index + 1), theme::secondary()),
                    Span::styled(station_label(state, *station_id), theme::primary_value()),
                    Span::styled(suffix, theme::secondary()),
                ])
            }),
    );
    frame.render_widget(
        Paragraph::new(stop_lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        stops_area,
    );
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

fn create_service_modal_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(6).min(88).max(48);
    let height = area.height.saturating_sub(2).min(24).max(16);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area.y.saturating_add(area.height.saturating_sub(height) / 2),
        width: width.min(area.width),
        height: height.min(area.height),
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

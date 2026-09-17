//! Secondary Passenger Service workspace opened from the Map.
//!
//! Passenger Services are persistent route patterns that can operate in one or
//! both directions. This workspace combines route definition with the live
//! operational view used to run and allocate the Fleet.

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
    model::{
        GameState, JourneyPurpose, PassengerService, RailStationId, ServiceDirectionMode,
        ServiceId, TrainStatus,
    },
    sim::demand::effective_arrival_rate_per_hour,
};

use super::{components::EmptyState, format, modal, theme};

mod assignment;
mod editor;

use assignment::{ServiceTrainAssignmentAction, ServiceTrainAssignmentFlow};
use editor::CreateServiceFlow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceWorkspaceAction {
    Continue,
    Close,
    RunService {
        service_id: ServiceId,
    },
    Create {
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
    },
    Update {
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
    },
    Rename {
        service_id: ServiceId,
        custom_name: Option<String>,
    },
    Delete {
        service_id: ServiceId,
    },
    AssignTrain {
        train_id: crate::model::TrainId,
        service_id: ServiceId,
    },
    UnassignTrain {
        train_id: crate::model::TrainId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ServiceNameEditor {
    service_id: ServiceId,
    draft: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ServiceNameEditorAction {
    Continue,
    Cancel,
    Confirm {
        service_id: ServiceId,
        custom_name: Option<String>,
    },
}

impl ServiceNameEditor {
    fn start(state: &GameState, service_id: ServiceId) -> Result<Self, String> {
        let service = state
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| "Passenger Service is no longer available.".to_owned())?;
        Ok(Self {
            service_id,
            draft: service.custom_name.clone().unwrap_or_default(),
            error: None,
        })
    }

    fn handle_key(&mut self, key: KeyCode) -> ServiceNameEditorAction {
        match key {
            KeyCode::Esc => ServiceNameEditorAction::Cancel,
            KeyCode::Enter => {
                let trimmed = self.draft.trim();
                ServiceNameEditorAction::Confirm {
                    service_id: self.service_id,
                    custom_name: (!trimmed.is_empty()).then(|| trimmed.to_owned()),
                }
            }
            KeyCode::Backspace => {
                self.draft.pop();
                self.error = None;
                ServiceNameEditorAction::Continue
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && self.draft.chars().count()
                        < PassengerService::MAX_CUSTOM_NAME_CHARACTERS =>
            {
                self.draft.push(character);
                self.error = None;
                ServiceNameEditorAction::Continue
            }
            KeyCode::Char(_) => {
                self.error = Some(format!(
                    "Name accepts up to {} visible characters.",
                    PassengerService::MAX_CUSTOM_NAME_CHARACTERS
                ));
                ServiceNameEditorAction::Continue
            }
            _ => ServiceNameEditorAction::Continue,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceWorkspace {
    open: bool,
    selected_service_index: usize,
    create_flow: Option<CreateServiceFlow>,
    assignment_flow: Option<ServiceTrainAssignmentFlow>,
    name_editor: Option<ServiceNameEditor>,
    delete_confirmation: Option<ServiceId>,
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
        if let Some(editor) = &mut self.name_editor {
            return match editor.handle_key(key) {
                ServiceNameEditorAction::Continue => ServiceWorkspaceAction::Continue,
                ServiceNameEditorAction::Cancel => {
                    self.name_editor = None;
                    ServiceWorkspaceAction::Continue
                }
                ServiceNameEditorAction::Confirm {
                    service_id,
                    custom_name,
                } => ServiceWorkspaceAction::Rename {
                    service_id,
                    custom_name,
                },
            };
        }

        if let Some(flow) = &mut self.assignment_flow {
            return match flow.handle_key(key, state) {
                ServiceTrainAssignmentAction::Continue => ServiceWorkspaceAction::Continue,
                ServiceTrainAssignmentAction::Cancel => {
                    self.assignment_flow = None;
                    ServiceWorkspaceAction::Continue
                }
                ServiceTrainAssignmentAction::Assign {
                    train_id,
                    service_id,
                } => ServiceWorkspaceAction::AssignTrain {
                    train_id,
                    service_id,
                },
                ServiceTrainAssignmentAction::Unassign { train_id } => {
                    ServiceWorkspaceAction::UnassignTrain { train_id }
                }
            };
        }

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

            return self
                .create_flow
                .as_mut()
                .expect("the creation flow was checked above")
                .handle_key(key, state);
        }

        match key {
            KeyCode::Esc => ServiceWorkspaceAction::Close,
            KeyCode::Enter => self
                .selected_service_id(state)
                .map(|service_id| ServiceWorkspaceAction::RunService { service_id })
                .unwrap_or(ServiceWorkspaceAction::Continue),
            KeyCode::Char('n' | 'N') => {
                self.create_flow = Some(CreateServiceFlow::new());
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Char('a' | 'A') => {
                if let Some(service_id) = self.selected_service_id(state) {
                    match ServiceTrainAssignmentFlow::start(state, service_id) {
                        Ok(flow) => self.assignment_flow = Some(flow),
                        Err(message) => self.reject_action(message),
                    }
                }
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Char('r' | 'R') => {
                if let Some(service_id) = self.selected_service_id(state) {
                    match ServiceNameEditor::start(state, service_id) {
                        Ok(editor) => self.name_editor = Some(editor),
                        Err(message) => self.reject_action(message),
                    }
                }
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
                            self.create_flow = Some(CreateServiceFlow::editing(
                                service.stop_station_ids.clone(),
                                selected_station_index,
                                service.direction_mode,
                                service_id,
                            ));
                        }
                    }
                }
                ServiceWorkspaceAction::Continue
            }
            KeyCode::Delete => {
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
        self.assignment_flow = None;
        self.name_editor = None;
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
        self.assignment_flow = None;
        self.name_editor = None;
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
        self.assignment_flow = None;
        self.name_editor = None;
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
            flow.reject(message);
        }
    }

    pub fn reject_assignment(&mut self, error: impl Into<String>) -> Option<String> {
        let error = error.into();
        if let Some(flow) = &mut self.assignment_flow {
            flow.reject(error);
            None
        } else {
            Some(error)
        }
    }

    pub fn reject_name(&mut self, error: impl Into<String>) -> Option<String> {
        let error = error.into();
        if let Some(editor) = &mut self.name_editor {
            editor.error = Some(error);
            None
        } else {
            Some(error)
        }
    }

    pub fn confirm_name_saved(&mut self) {
        self.name_editor = None;
    }

    pub fn confirm_assignment_saved(&mut self) {
        self.assignment_flow = None;
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

        if let Some(flow) = &self.assignment_flow {
            return flow.footer_shortcuts(state, compact);
        }

        if self.name_editor.is_some() {
            return vec![
                ("Enter", "Save", true),
                ("Backspace", "Erase", true),
                ("Esc", "Cancel", true),
            ];
        }

        if let Some(flow) = &self.create_flow {
            return flow.footer_shortcuts(compact);
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
            ("Enter", "Run", has_services),
            ("N", "New", true),
            (
                "A",
                "Assign train",
                has_services && !state.player_company.fleet.trains.is_empty(),
            ),
            ("R", "Name", has_services),
            ("E", "Edit", can_edit),
            ("Del", "Delete", can_delete),
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
                "n Create the first Passenger Service".into(),
                "New Services start bidirectional; press M in the editor for one-way operation."
                    .into(),
                "Esc Return to Map".into(),
            ]);
        } else {
            lines.extend([
                "↑↓ / jk Select Passenger Service".into(),
                "PgUp / PgDn Move through longer Service lists".into(),
                "Enter Run the selected Service with a READY Train".into(),
                "n Create a new Passenger Service".into(),
                "a Assign, reassign, or unassign Trains for the selected Service".into(),
                "r Set or clear the selected Service's commercial name".into(),
                "e Edit the selected Service when it has no active Journeys".into(),
                "Del Delete the selected Service when it has no active Journeys".into(),
                "Esc Return to Map".into(),
            ]);
        }
        lines.extend([
            String::new(),
            "During create/edit: ↑↓/jk moves; Space toggles a stop; M changes direction; Enter reviews."
                .into(),
            "During review: Enter creates/saves; Left or Backspace returns to editing.".into(),
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
        self.create_flow.is_some()
            || self.assignment_flow.is_some()
            || self.name_editor.is_some()
            || self.delete_confirmation.is_some()
    }

    /// Renders only the persistent Passenger Services workspace.  The shell
    /// draws focused modals in a later layer so it can dim the entire
    /// application underneath them, including the header and footer.
    pub fn render_base(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_service_list(frame, area, state, self.selected_service_index);
    }

    /// Renders the currently focused Passenger Services modal, if any.
    pub fn render_modal(&self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(flow) = &self.assignment_flow {
            assignment::render(frame, modal::workflow_rect(area), state, flow);
        } else if let Some(editor) = &self.name_editor {
            render_name_editor(frame, area, state, editor);
        } else if let Some(flow) = &self.create_flow {
            // Route editing benefits from enough room to preserve the same
            // geographic topology and labels as the operational Map. Keep the
            // normal workflow size for the other focused modals.
            editor::render(frame, modal::centered_rect(area, 118, 30), state, flow);
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
        EmptyState::first_use(
            "No Passenger Services",
            "Create a reusable route, assign trains, and run it directly from this workspace.",
            "N",
            "Create service",
        )
        .motif("●━━●━━●")
        .hint("Services operate both directions by default; one-way operation is optional.")
        .render(frame, content);
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
            let route = service_route_label(state, service);
            if wide {
                let snapshot = service_operating_snapshot(state, service.id);
                Row::new(vec![
                    Cell::from(service.display_name()),
                    Cell::from(route),
                    Cell::from(snapshot.assigned_trains.to_string()),
                    Cell::from(snapshot.runnable_assigned_trains.to_string()),
                    Cell::from(snapshot.active_trains.to_string()),
                    Cell::from(snapshot.waiting_passengers.to_string()),
                ])
            } else {
                Row::new([service.display_name(), route])
            }
        })
        .collect::<Vec<_>>();

    let (header, widths) = if wide {
        (
            Row::new([
                "Service", "Route", "Assigned", "Ready", "Running", "Waiting",
            ])
            .style(theme::table_header())
            .bottom_margin(1),
            vec![
                Constraint::Length(8),
                Constraint::Min(12),
                Constraint::Length(8),
                Constraint::Length(5),
                Constraint::Length(7),
                Constraint::Length(7),
            ],
        )
    } else {
        (
            Row::new(["Service", "Route"])
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
    let directional_demand = service_directional_demand(state, service);
    let (state_label, state_style) = service_state_label(&snapshot);
    let origin = service
        .origin_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let destination = service
        .destination_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());

    let mut lines = vec![Line::styled(service.display_name(), theme::focused_title())];

    if tight {
        lines.push(Line::styled(state_label.to_owned(), state_style));
    } else {
        inspector_section(&mut lines, "STATUS", dense);
        lines.push(labelled_line_styled("State", state_label, state_style));
        lines.push(labelled_line(
            "Fleet",
            &format!(
                "{} assigned · {} ready · {} running",
                snapshot.assigned_trains, snapshot.runnable_assigned_trains, snapshot.active_trains
            ),
        ));
    }

    inspector_section(&mut lines, "ROUTE", dense);
    lines.push(labelled_line(
        "Pattern",
        &truncate_display(&service_route_label(state, service), value_width),
    ));
    if !tight {
        lines.push(labelled_line("Distance", &format::distance(distance)));
        lines.push(labelled_line(
            "Stops",
            &service.stop_station_ids.len().to_string(),
        ));
    }

    inspector_section(&mut lines, "TRAIN NUMBERS", dense);
    if density == ServiceInspectorDensity::Full {
        lines.push(Line::from(format!(
            "{}  {} → {}",
            service.forward_train_number, origin, destination
        )));
        if let Some(reverse_train_number) = service.reverse_train_number {
            lines.push(Line::from(format!(
                "{}  {} → {}",
                reverse_train_number, destination, origin
            )));
        }
    } else {
        let numbers = service
            .reverse_train_number
            .map(|reverse| format!("{} / {reverse}", service.forward_train_number))
            .unwrap_or_else(|| service.forward_train_number.to_string());
        lines.push(labelled_line("Numbers", &numbers));
    }

    inspector_section(&mut lines, "DEMAND", dense);
    lines.push(labelled_line(
        &truncate_display(&format!("→ {destination}"), 15),
        &format!(
            "{} · +{}/h",
            directional_demand.forward_waiting, directional_demand.forward_rate_per_hour
        ),
    ));
    if service.direction_mode == ServiceDirectionMode::BothDirections {
        lines.push(labelled_line(
            &truncate_display(&format!("→ {origin}"), 15),
            &format!(
                "{} · +{}/h",
                directional_demand.reverse_waiting, directional_demand.reverse_rate_per_hour
            ),
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
    if snapshot.active_trains > 0 {
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

    if density == ServiceInspectorDensity::Full && snapshot.assigned_trains > 0 {
        inspector_section(&mut lines, "ASSIGNED FLEET", false);
        lines.extend(service_assigned_fleet_lines(state, service, width, 3));
        if snapshot.assigned_trains > 3 {
            lines.push(Line::styled(
                format!("… +{} more assigned", snapshot.assigned_trains - 3),
                theme::secondary(),
            ));
        }
    }

    if density == ServiceInspectorDensity::Full && snapshot.active_trains > 0 {
        inspector_section(&mut lines, "RUNNING TRAINS", false);
        for train in snapshot.running_trains.iter().take(2) {
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
        if snapshot.running_trains.len() > 2 {
            lines.push(Line::styled(
                format!("… +{} more running", snapshot.running_trains.len() - 2),
                theme::secondary(),
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

    if density == ServiceInspectorDensity::Full
        && snapshot.active_trains == 0
        && snapshot.assigned_trains == 0
    {
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

fn service_state_label(
    snapshot: &ServiceOperatingSnapshot,
) -> (&'static str, ratatui::style::Style) {
    if snapshot.active_trains > 0 {
        ("LIVE", theme::success())
    } else if snapshot.runnable_assigned_trains > 0 {
        ("READY", theme::success())
    } else if snapshot.assigned_trains > 0 {
        ("POSITION", theme::warning())
    } else {
        ("IDLE", theme::secondary())
    }
}

fn service_assigned_fleet_lines(
    state: &GameState,
    service: &PassengerService,
    width: usize,
    limit: usize,
) -> Vec<Line<'static>> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| {
            state.player_company.fleet.assigned_service_id(train.id) == Some(service.id)
        })
        .take(limit)
        .map(|train| {
            let label = train
                .nickname
                .as_ref()
                .map(|nickname| nickname.as_str().to_owned())
                .unwrap_or_else(|| format!("Train {:02}", train.id.get()));
            let (status, detail, style) = match train.status {
                TrainStatus::Ready { at } if service_accepts_departure(service, at) => {
                    ("READY", station_label(state, at), theme::success())
                }
                TrainStatus::Ready { at } => {
                    ("POSITION", station_label(state, at), theme::warning())
                }
                TrainStatus::Travelling { journey_id } => {
                    let journey = state
                        .active_journeys
                        .iter()
                        .find(|journey| journey.id == journey_id);
                    let destination = journey
                        .map(|journey| {
                            format!("→ {}", station_label(state, journey.destination_station_id))
                        })
                        .unwrap_or_else(|| "Journey in progress".into());
                    if journey.is_some_and(|journey| journey.purpose == JourneyPurpose::Positioning)
                    {
                        ("POSITIONING", destination, theme::warning())
                    } else {
                        ("RUNNING", destination, theme::primary_value())
                    }
                }
            };
            Line::from(vec![
                Span::styled(
                    truncate_display(&label, width.saturating_sub(18).max(8)),
                    theme::primary_value(),
                ),
                Span::raw("  "),
                Span::styled(status.to_owned(), style),
                Span::styled(format!(" · {detail}"), theme::secondary()),
            ])
        })
        .collect()
}

fn service_accepts_departure(service: &PassengerService, station_id: RailStationId) -> bool {
    service.origin_station_id() == Some(station_id)
        || (service.direction_mode == ServiceDirectionMode::BothDirections
            && service.destination_station_id() == Some(station_id))
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
    assigned_trains: usize,
    runnable_assigned_trains: usize,
    waiting_passengers: u32,
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
    snapshot.waiting_passengers = service_waiting_demand(state, service);

    for train in &state.player_company.fleet.trains {
        if state.player_company.fleet.assigned_service_id(train.id) != Some(service_id) {
            continue;
        }
        snapshot.assigned_trains = snapshot.assigned_trains.saturating_add(1);
        if let TrainStatus::Ready { at } = train.status
            && service_accepts_departure(service, at)
        {
            snapshot.runnable_assigned_trains = snapshot.runnable_assigned_trains.saturating_add(1);
        }
    }

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
        let next_station_id = service_journey_direction(service, journey)
            .and_then(|direction| next_service_stop_index(journey.current_stop_index, direction))
            .and_then(|index| service.stop_station_ids.get(index).copied())
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ServiceDirectionalDemandSnapshot {
    forward_waiting: u32,
    forward_rate_per_hour: u32,
    reverse_waiting: u32,
    reverse_rate_per_hour: u32,
}

fn service_directional_demand(
    state: &GameState,
    service: &PassengerService,
) -> ServiceDirectionalDemandSnapshot {
    let mut snapshot = ServiceDirectionalDemandSnapshot::default();

    for (origin_index, origin_station_id) in service.stop_station_ids.iter().enumerate() {
        for (destination_index, destination_station_id) in
            service.stop_station_ids.iter().enumerate()
        {
            let direction = if destination_index > origin_index {
                Some(1)
            } else if service.direction_mode == ServiceDirectionMode::BothDirections
                && destination_index < origin_index
            {
                Some(-1)
            } else {
                None
            };
            let Some(direction) = direction else {
                continue;
            };

            let Some(demand) = state.origin_destination_demand.iter().find(|demand| {
                demand.origin_station_id == *origin_station_id
                    && demand.destination_station_id == *destination_station_id
            }) else {
                continue;
            };
            let rate = effective_arrival_rate_per_hour(state, demand);
            if direction > 0 {
                snapshot.forward_waiting = snapshot
                    .forward_waiting
                    .saturating_add(demand.waiting_passengers);
                snapshot.forward_rate_per_hour =
                    snapshot.forward_rate_per_hour.saturating_add(rate);
            } else {
                snapshot.reverse_waiting = snapshot
                    .reverse_waiting
                    .saturating_add(demand.waiting_passengers);
                snapshot.reverse_rate_per_hour =
                    snapshot.reverse_rate_per_hour.saturating_add(rate);
            }
        }
    }

    snapshot
}

fn service_waiting_demand(state: &GameState, service: &PassengerService) -> u32 {
    let directional = service_directional_demand(state, service);
    directional
        .forward_waiting
        .saturating_add(directional.reverse_waiting)
}

fn service_journey_direction(
    service: &PassengerService,
    journey: &crate::model::Journey,
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

fn next_service_stop_index(current_stop_index: usize, direction: i32) -> Option<usize> {
    match direction {
        1 => current_stop_index.checked_add(1),
        -1 => current_stop_index.checked_sub(1),
        _ => None,
    }
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

fn render_name_editor(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    editor: &ServiceNameEditor,
) {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == editor.service_id)
    else {
        return;
    };

    let card_height = if editor.error.is_some() { 16 } else { 15 };
    let card = modal::editor_rect(area, card_height);
    let footer = if card.width >= 56 {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Backspace", modal::ModalAction::Erase),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    };
    let modal_areas = modal::render_shell(frame, card, "Name Passenger Service", footer);

    let draft = if editor.draft.is_empty() {
        "(no commercial name)".to_owned()
    } else {
        editor.draft.clone()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Service  ", theme::secondary()),
            Span::styled(service.name.clone(), theme::focused_title()),
        ]),
        Line::styled(service_route_label(state, service), theme::secondary()),
        Line::from(""),
        Line::styled("COMMERCIAL NAME", theme::table_header()),
        Line::styled(draft, theme::primary_value()),
        Line::from(""),
        Line::styled(
            "Shared by both directions. Leave empty to show only the generated Service code.",
            theme::secondary(),
        ),
    ];
    if let Some(error) = &editor.error {
        lines.push(Line::from(""));
        lines.push(Line::styled(error.clone(), theme::error()));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
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
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Delete),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    } else {
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )])
    };
    let modal_areas = modal::render_shell(frame, card, "Delete Passenger Service", footer);

    let mut lines = vec![
        Line::styled(service.display_name(), theme::focused_title()),
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

fn service_route_label(state: &GameState, service: &PassengerService) -> String {
    let origin = service
        .origin_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let destination = service
        .destination_station_id()
        .map(|station_id| station_label(state, station_id))
        .unwrap_or_else(|| "Unknown".into());
    let arrow = match service.direction_mode {
        ServiceDirectionMode::BothDirections => "↔",
        ServiceDirectionMode::ForwardOnly => "→",
    };
    format!("{origin} {arrow} {destination}")
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

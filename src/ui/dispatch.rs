//! Keyboard state and text presentation for a Manual Dispatch.
//!
//! Manual Dispatch operates only over persistent Passenger Services. The flow
//! keeps a proposed READY Train and Service in presentation state; the
//! application boundary revalidates both before authorising the Journey.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Gauge, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
    },
};

use crate::{
    catalog::model_for_train,
    model::{GameState, Money, RailStationId, ServiceId, TrainId, TrainStatus},
    sim::economy::{JourneyQuote, quote_journey},
    ui::{modal, theme},
};

/// The result of handling a key within the shared Manual Dispatch flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchFlowAction {
    /// The player is still reviewing or selecting the dispatch.
    Continue,
    /// The player abandoned the flow without authorising a Journey.
    Cancel,
    /// The application boundary must revalidate and authorise this dispatch.
    Confirm {
        train_id: TrainId,
        service_id: ServiceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DispatchStep {
    SelectTrain {
        selected_train_id: Option<TrainId>,
        table_state: TableState,
        page_size: usize,
    },
    SelectService {
        train_id: TrainId,
        selected_service_id: Option<ServiceId>,
        table_state: TableState,
        page_size: usize,
    },
    Confirm {
        train_id: TrainId,
        service_id: ServiceId,
        quote: JourneyQuote,
    },
}

/// Presentation state for one uncommitted Manual Dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchFlow {
    step: DispatchStep,
    preferred_station_id: Option<RailStationId>,
    preferred_service_id: Option<ServiceId>,
    rejection: Option<String>,
}

/// Presentation-only owner of the active Manual Dispatch workflow.
///
/// The shell only needs to know whether dispatch is active and whether a
/// cancelled flow should return to Fleet. The concrete step state remains
/// inside this workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DispatchWorkspace {
    flow: Option<DispatchFlow>,
    returns_to_fleet: bool,
}

/// One contextual footer action owned by the Manual Dispatch workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl DispatchShortcut {
    fn enabled(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            enabled: true,
        }
    }
}

impl DispatchWorkspace {
    pub fn has_flow(&self) -> bool {
        self.flow.is_some()
    }

    pub fn flow(&self) -> Option<&DispatchFlow> {
        self.flow.as_ref()
    }

    pub fn flow_mut(&mut self) -> Option<&mut DispatchFlow> {
        self.flow.as_mut()
    }

    pub fn start_from_map(&mut self, state: &GameState) -> Result<(), &'static str> {
        self.flow = Some(DispatchFlow::start(state)?);
        self.returns_to_fleet = false;
        Ok(())
    }

    pub fn start_from_fleet(&mut self, state: &GameState, train_id: TrainId) -> Result<(), String> {
        self.flow = Some(DispatchFlow::start_with_selected_train(state, train_id)?);
        self.returns_to_fleet = true;
        Ok(())
    }

    pub fn start_from_service(
        &mut self,
        state: &GameState,
        service_id: ServiceId,
    ) -> Result<(), String> {
        self.flow = Some(DispatchFlow::start_with_selected_service(
            state, service_id,
        )?);
        self.returns_to_fleet = false;
        Ok(())
    }

    /// Clears the workflow and reports whether Fleet was its launch context.
    pub fn close(&mut self) -> bool {
        self.flow = None;
        std::mem::take(&mut self.returns_to_fleet)
    }

    pub fn reset(&mut self) {
        self.flow = None;
        self.returns_to_fleet = false;
    }

    /// Returns contextual footer actions for the active dispatch step.
    pub fn shortcuts(&self, compact: bool, wide: bool) -> Option<Vec<DispatchShortcut>> {
        let flow = self.flow.as_ref()?;
        if flow.is_selecting_train() {
            return Some(vec![
                DispatchShortcut::enabled(if compact { "↑↓" } else { "↑↓/JK" }, "Train"),
                DispatchShortcut::enabled(
                    "Enter",
                    if flow.preferred_service_id.is_some() {
                        "Review"
                    } else {
                        "Next"
                    },
                ),
                DispatchShortcut::enabled("Esc", "Cancel"),
            ]);
        }

        if flow.is_selecting_service() {
            let mut items = vec![DispatchShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Service",
            )];
            if wide {
                items.push(DispatchShortcut::enabled("PgUp/PgDn", "Scroll"));
            }
            items.push(DispatchShortcut::enabled("Enter", "Review"));
            items.push(DispatchShortcut::enabled("←", "Back"));
            items.push(DispatchShortcut::enabled("Esc", "Cancel"));
            return Some(items);
        }

        Some(vec![
            DispatchShortcut::enabled("Enter", "Confirm"),
            DispatchShortcut::enabled(
                "←",
                if flow.preferred_service_id.is_some() {
                    "Train"
                } else {
                    "Back"
                },
            ),
            DispatchShortcut::enabled("Esc", "Cancel"),
        ])
    }

    /// Returns help content for the active dispatch step.
    pub fn help_lines(&self) -> Option<Vec<String>> {
        let flow = self.flow.as_ref()?;
        let mut lines = vec!["Current · Manual Dispatch".into()];
        if flow.is_selecting_train() {
            lines.extend([
                "↑↓ / jk Select a READY Train".into(),
                if flow.preferred_service_id.is_some() {
                    "Enter Review this Service run".into()
                } else {
                    "Enter Continue to Service selection".into()
                },
                "Esc Cancel dispatch".into(),
            ]);
        } else if flow.is_selecting_service() {
            lines.extend([
                "↑↓ / jk Select a Passenger Service from this station".into(),
                "PgUp / PgDn Scroll longer route lists".into(),
                "Enter Review Journey".into(),
                "← / Backspace Previous step   Esc Cancel".into(),
            ]);
        } else {
            lines.extend([
                "Enter Confirm dispatch".into(),
                "← / Backspace Previous step   Esc Cancel".into(),
            ]);
        }
        lines.extend([
            String::new(),
            "The footer always shows the actions available in the current step.".into(),
        ]);
        Some(lines)
    }
}

impl DispatchFlow {
    /// Returns the active step so the shell can publish only controls this
    /// flow actually handles.
    pub fn is_selecting_train(&self) -> bool {
        matches!(self.step, DispatchStep::SelectTrain { .. })
    }

    /// Returns whether the flow is choosing a persistent Passenger Service.
    pub fn is_selecting_service(&self) -> bool {
        matches!(self.step, DispatchStep::SelectService { .. })
    }

    /// Returns whether Enter currently confirms the quoted Journey.
    pub fn is_confirming(&self) -> bool {
        matches!(self.step, DispatchStep::Confirm { .. })
    }

    /// Starts company-wide Manual Dispatch by listing every READY Train.
    ///
    /// The selected Train determines the Journey origin from its current
    /// Rail Station. No game state changes at this point.
    pub fn start(state: &GameState) -> Result<Self, &'static str> {
        Self::start_at_station(state, None)
    }

    /// Starts a contextual dispatch flow at one focused Rail Station and
    /// limits the chooser to READY Trains physically based there.
    ///
    /// The Map's primary `D` shortcut uses company-wide [`Self::start`];
    /// this station-scoped entry point remains available for contextual actions.
    pub fn start_at_station(
        state: &GameState,
        preferred_station_id: Option<RailStationId>,
    ) -> Result<Self, &'static str> {
        let ready_train_ids = ready_train_ids_for_station(state, preferred_station_id);
        if ready_train_ids.is_empty() {
            return Err(match preferred_station_id {
                Some(_) => "No READY Train is available at the selected Rail Station.",
                None => no_ready_train_reason(state),
            });
        }
        let train_ids = dispatchable_train_ids_for_context(state, preferred_station_id, None);
        if train_ids.is_empty() {
            let any_assigned = ready_train_ids.iter().any(|train_id| {
                state
                    .player_company
                    .fleet
                    .assigned_service_id(*train_id)
                    .is_some()
            });
            return Err(if !any_assigned {
                "No READY Train is assigned to a Passenger Service. Assign one from Fleet or Services before revenue dispatch."
            } else {
                match preferred_station_id {
                    Some(_) => {
                        "No assigned Passenger Service can be operated from this Rail Station by a READY Train."
                    }
                    None => {
                        "No assigned Passenger Service can be operated from any READY Train's current Rail Station."
                    }
                }
            });
        }
        Ok(Self {
            step: train_selection_step(state, Some(train_ids[0]), preferred_station_id, None),
            preferred_station_id,
            preferred_service_id: None,
            rejection: None,
        })
    }

    /// Starts the same Manual Dispatch workflow used everywhere, with one Fleet
    /// Train initially focused in the Train step. The player may still choose a
    /// different READY Train before continuing.
    pub fn start_with_selected_train(state: &GameState, train_id: TrainId) -> Result<Self, String> {
        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == train_id)
            .ok_or_else(|| format!("Train {} is no longer in the Fleet.", train_id.get()))?;
        if matches!(train.status, TrainStatus::Travelling { .. }) {
            return Err(format!(
                "Train {} is TRAVELLING and cannot be dispatched until its Journey arrives.",
                train.id.get()
            ));
        }
        if state
            .player_company
            .fleet
            .assigned_service_id(train_id)
            .is_none()
        {
            return Err(format!(
                "Train {} is UNASSIGNED. Assign a Passenger Service from Fleet or Services before revenue dispatch.",
                train.id.get()
            ));
        }
        if service_options(state, train_id).is_empty() {
            return Err(
                "This Train's assigned Passenger Service cannot be operated from its current Rail Station."
                    .into(),
            );
        }

        Ok(Self {
            step: train_selection_step(state, Some(train_id), None, None),
            preferred_station_id: None,
            preferred_service_id: None,
            rejection: None,
        })
    }

    /// Starts Manual Dispatch from Passenger Services with the selected Service
    /// fixed for the workflow. The player chooses a READY Train that can run
    /// the Service from its current terminus, then reviews the Journey quote.
    pub fn start_with_selected_service(
        state: &GameState,
        service_id: ServiceId,
    ) -> Result<Self, String> {
        let service = state
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| {
                format!(
                    "Passenger Service R{} is no longer available.",
                    service_id.get()
                )
            })?;
        let train_ids = dispatchable_train_ids_for_context(state, None, Some(service_id));
        if train_ids.is_empty() {
            return Err(format!(
                "No assigned READY Train can run {} from a valid terminus.",
                service_name(state, service.id)
            ));
        }

        Ok(Self {
            step: train_selection_step(state, Some(train_ids[0]), None, Some(service_id)),
            preferred_station_id: None,
            preferred_service_id: Some(service_id),
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> DispatchFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return DispatchFlowAction::Cancel;
        }

        match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                page_size,
            } => {
                let trains = dispatchable_train_ids_for_context(
                    state,
                    self.preferred_station_id,
                    self.preferred_service_id,
                );
                if trains.is_empty() {
                    self.rejection = Some(if let Some(service_id) = self.preferred_service_id {
                        format!(
                            "No assigned READY Train can currently run {} from a valid terminus.",
                            service_name(state, service_id)
                        )
                    } else {
                        match self.preferred_station_id {
                            Some(_) => {
                                "No READY Train is available at the selected Rail Station.".into()
                            }
                            None => no_ready_train_reason(state).into(),
                        }
                    });
                    return DispatchFlowAction::Continue;
                }
                if synchronize_train_selection(selected_train_id, table_state, &trains) {
                    self.rejection = Some(
                        "The previously selected Train is no longer dispatchable; choose another READY Train."
                            .into(),
                    );
                }
                move_train_selection(
                    selected_train_id,
                    table_state,
                    &trains,
                    *page_size,
                    key.code,
                );
                if matches!(key.code, KeyCode::Enter) {
                    let Some(train_id) = *selected_train_id else {
                        if self.rejection.is_none() {
                            self.rejection = Some("Select a READY Train before continuing.".into());
                        }
                        return DispatchFlowAction::Continue;
                    };
                    if ready_train_station(state, train_id).is_none() {
                        self.rejection =
                            Some("That Train is no longer READY; select a Train again.".into());
                        return DispatchFlowAction::Continue;
                    }
                    if let Some(service_id) = self.preferred_service_id {
                        match preview_quote(state, train_id, service_id) {
                            Ok(quote) => {
                                self.step = DispatchStep::Confirm {
                                    train_id,
                                    service_id,
                                    quote,
                                };
                                self.rejection = None;
                            }
                            Err(error) => self.rejection = Some(error),
                        }
                    } else {
                        match service_step(state, train_id, None) {
                            Ok(step) => {
                                self.step = step;
                                self.rejection = None;
                            }
                            Err(error) => self.rejection = Some(error),
                        }
                    }
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::SelectService {
                train_id,
                selected_service_id,
                table_state,
                page_size,
            } => {
                if matches!(key.code, KeyCode::Left | KeyCode::Backspace) {
                    let selected_train_id = *train_id;
                    self.step = train_selection_step(
                        state,
                        Some(selected_train_id),
                        self.preferred_station_id,
                        self.preferred_service_id,
                    );
                    self.rejection = None;
                    return DispatchFlowAction::Continue;
                }
                if ready_train_station(state, *train_id).is_none() {
                    self.rejection = Some(
                        "That Train is no longer READY; Left or Backspace returns to Train selection."
                            .into(),
                    );
                    return DispatchFlowAction::Continue;
                }
                let services = service_options(state, *train_id);
                if services.is_empty() {
                    self.rejection = Some(
                        "No Passenger Service can be operated from this Train's current Rail Station. Create one from the Map Services workspace first."
                            .into(),
                    );
                    return DispatchFlowAction::Continue;
                }
                if synchronize_service_selection(selected_service_id, table_state, &services) {
                    self.rejection = Some(
                        "The selected Passenger Service is no longer dispatchable; choose another Service."
                            .into(),
                    );
                }
                move_service_selection(
                    selected_service_id,
                    table_state,
                    &services,
                    *page_size,
                    key.code,
                );
                if matches!(key.code, KeyCode::Enter) {
                    let Some(service_id) = *selected_service_id else {
                        self.rejection = Some(
                            "Select a Passenger Service before continuing, or return to Train selection."
                                .into(),
                        );
                        return DispatchFlowAction::Continue;
                    };
                    if let Some(service) = services
                        .iter()
                        .find(|service| service.service_id == service_id)
                    {
                        self.step = DispatchStep::Confirm {
                            train_id: *train_id,
                            service_id,
                            quote: service.quote.clone(),
                        };
                        self.rejection = None;
                    } else {
                        self.rejection = Some(
                            "The selected Passenger Service changed; choose a Service again."
                                .into(),
                        );
                    }
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::Confirm {
                train_id,
                service_id,
                quote,
            } => {
                if matches!(key.code, KeyCode::Left | KeyCode::Backspace) {
                    if self.preferred_service_id.is_some() {
                        self.step = train_selection_step(
                            state,
                            Some(*train_id),
                            self.preferred_station_id,
                            self.preferred_service_id,
                        );
                        self.rejection = None;
                    } else {
                        match service_step(state, *train_id, Some(*service_id)) {
                            Ok(step) => {
                                self.step = step;
                                self.rejection = None;
                            }
                            Err(error) => self.rejection = Some(error),
                        }
                    }
                    DispatchFlowAction::Continue
                } else if matches!(key.code, KeyCode::Enter) {
                    match preview_quote(state, *train_id, *service_id) {
                        Ok(current_quote) if current_quote != *quote => {
                            *quote = current_quote;
                            self.rejection = Some(
                                "Journey quote updated from current conditions. Review the revised consequences and press Enter again."
                                    .into(),
                            );
                            DispatchFlowAction::Continue
                        }
                        Ok(_) => DispatchFlowAction::Confirm {
                            train_id: *train_id,
                            service_id: *service_id,
                        },
                        Err(error) => {
                            self.rejection = Some(format!(
                                "Journey quote could not be revalidated: {error}. Your choices are still available."
                            ));
                            DispatchFlowAction::Continue
                        }
                    }
                } else {
                    DispatchFlowAction::Continue
                }
            }
        }
    }

    /// Records an application-boundary rejection while keeping the proposal
    /// open so the concrete cause remains visible to the player.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the current keyboard step and the uncommitted Journey quote.
    pub fn render(&self, state: &GameState) -> String {
        let mut output = String::from("Manual Dispatch\n");
        match &self.step {
            DispatchStep::SelectTrain {
                selected_train_id, ..
            } => {
                output.push_str("Select a READY Train (Up/Down, Enter; Esc cancels):\n");
                for train_id in dispatchable_train_ids_for_context(
                    state,
                    self.preferred_station_id,
                    self.preferred_service_id,
                ) {
                    let marker = if Some(train_id) == *selected_train_id {
                        '>'
                    } else {
                        ' '
                    };
                    let Some(train) = state
                        .player_company
                        .fleet
                        .trains
                        .iter()
                        .find(|train| train.id == train_id)
                    else {
                        continue;
                    };
                    let location = ready_train_station(state, train_id)
                        .map(|station_id| station_label(state, station_id))
                        .unwrap_or("unknown Rail Station");
                    output.push_str(&format!(
                        " {marker} Train {} ({}) at {location}\n",
                        train.id.get(),
                        train_model_name(state, train.id)
                    ));
                }
            }
            DispatchStep::SelectService {
                train_id,
                selected_service_id,
                ..
            } => {
                let origin = ready_train_station(state, *train_id);
                output.push_str(&format!(
                    "Select a Passenger Service for Train {} from {} (Up/Down, Enter; Left/Backspace returns to Train; Esc cancels):\n",
                    train_id.get(),
                    origin
                        .map(|station_id| station_label(state, station_id))
                        .unwrap_or("unknown Rail Station")
                ));
                for service in service_options(state, *train_id) {
                    let marker = if Some(service.service_id) == *selected_service_id {
                        '>'
                    } else {
                        ' '
                    };
                    output.push_str(&format!(
                        " {marker} {} — {} — {} waiting, {}, {}\n",
                        service_name(state, service.service_id),
                        service_route_label(state, service.service_id),
                        waiting_passengers_for_service(state, service.service_id),
                        format_distance(service.quote.distance.metres()),
                        format_duration(service.quote.duration.seconds()),
                    ));
                }
            }
            DispatchStep::Confirm {
                service_id, quote, ..
            } => {
                output.push_str(&format!(
                    "Passenger Service {}: {} ({} Rail Lines)\n",
                    service_name(state, *service_id),
                    service_route_label(state, *service_id),
                    quote.rail_line_path.len(),
                ));
                output.push_str(&format!(
                    "Directional Demand: {} Waiting Passengers; {} boarding / {} capacity\n",
                    waiting_passengers_for_service(state, quote.service_id),
                    quote.boarded_passengers,
                    train_capacity(state, quote.train_id),
                ));
                output.push_str(&format!(
                    "Route: {} | Duration: {} | Revenue booked at origin: {}\n",
                    format_path(state, quote),
                    format_duration(quote.duration.seconds()),
                    format_money(quote.operating_revenue),
                ));
                if quote.infrastructure_access_fee_credit > Money::ZERO {
                    output.push_str(&format!(
                        "Infrastructure Access Fee: {} - {} credit = {} | Fuel Cost: {} | Paid-now total: {}\n",
                        format_money(quote.infrastructure_access_fee_before_credit),
                        format_money(quote.infrastructure_access_fee_credit),
                        format_money(quote.infrastructure_access_fee),
                        format_money(quote.fuel_cost),
                        format_money(quote.operating_cost),
                    ));
                } else {
                    output.push_str(&format!(
                        "Infrastructure Access Fee: {} | Fuel Cost: {} | Paid-now total: {}\n",
                        format_money(quote.infrastructure_access_fee),
                        format_money(quote.fuel_cost),
                        format_money(quote.operating_cost),
                    ));
                }
                output.push_str(&format!(
                    "Estimated profit: {} | Company Funds after departure: {}\n",
                    format_money(quote.journey_profitability),
                    format_money(quote.cash_after_cost),
                ));
                if quote.boarded_passengers == 0 {
                    if service_stop_count(state, *service_id) > 2 {
                        output.push_str(
                            "NO ORIGIN BOARDING: later Service stops may still board passengers.\n",
                        );
                    } else {
                        output.push_str(
                            "EMPTY REPOSITIONING: no passenger revenue is currently booked.\n",
                        );
                    }
                }
                if quote.cash_after_cost < Money::ZERO {
                    output.push_str(
                        "INSUFFICIENT FUNDS: paid-now total exceeds current Company Funds.\n",
                    );
                }
                output.push_str("Enter confirms Manual Dispatch (revalidated); Esc cancels.\n");
            }
        }
        if let Some(rejection) = &self.rejection {
            output.push_str(&format!("Departure rejected: {rejection}\n"));
        }
        output
    }

    /// Renders the stateful Manual Dispatch flow without changing the game.
    pub fn render_panel(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        let selection_changed = match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                ..
            } => synchronize_train_selection(
                selected_train_id,
                table_state,
                &dispatchable_train_ids_for_context(
                    state,
                    self.preferred_station_id,
                    self.preferred_service_id,
                ),
            ),
            DispatchStep::SelectService { .. } | DispatchStep::Confirm { .. } => false,
        };
        if selection_changed {
            self.rejection = Some(
                "The previously selected Train is no longer dispatchable; choose a READY Train with an available Service."
                    .into(),
            );
        }

        let modal_areas = modal::render_shell(
            frame,
            area,
            "Manual Dispatch",
            dispatch_footer_line(&self.step, area.width, self.preferred_service_id.is_some()),
        );

        match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                page_size,
            } => render_train_chooser(
                frame,
                modal_areas.body,
                TrainChooserContext {
                    state,
                    preferred_station_id: self.preferred_station_id,
                    preferred_service_id: self.preferred_service_id,
                    selected_train_id: *selected_train_id,
                    rejection: self.rejection.as_deref(),
                },
                table_state,
                page_size,
            ),
            DispatchStep::SelectService {
                train_id,
                selected_service_id,
                table_state,
                page_size,
            } => render_service_chooser(
                frame,
                modal_areas.body,
                ServiceChooserContext {
                    state,
                    train_id: *train_id,
                    rejection: self.rejection.as_deref(),
                },
                selected_service_id,
                table_state,
                page_size,
            ),
            DispatchStep::Confirm { .. } => {
                let DispatchStep::Confirm {
                    service_id, quote, ..
                } = &self.step
                else {
                    unreachable!("Manual Dispatch review requires a Journey quote");
                };
                render_quote_review(
                    frame,
                    modal_areas.body,
                    state,
                    *service_id,
                    quote,
                    self.rejection.as_deref(),
                    self.preferred_service_id.is_some(),
                );
            }
        }
    }
}

fn dispatch_footer_line(
    step: &DispatchStep,
    width: u16,
    service_preselected: bool,
) -> Line<'static> {
    let select_train_action = if service_preselected {
        modal::ModalAction::Review
    } else {
        modal::ModalAction::Service
    };
    let confirm_back_action = if service_preselected {
        modal::ModalAction::Train
    } else {
        modal::ModalAction::Service
    };

    match step {
        DispatchStep::SelectTrain { .. } if width >= 76 => modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑/↓", modal::ModalAction::Choose),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("Enter", select_train_action),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
        DispatchStep::SelectTrain { .. } => modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑/↓", modal::ModalAction::Choose),
            modal::ModalShortcut::enabled("Enter", select_train_action),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
        DispatchStep::SelectService { .. } if width >= 82 => modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑/↓", modal::ModalAction::Choose),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
            modal::ModalShortcut::enabled("←", modal::ModalAction::Train),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
        DispatchStep::SelectService { .. } => modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑/↓", modal::ModalAction::Choose),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
            modal::ModalShortcut::enabled("←", modal::ModalAction::Train),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
        DispatchStep::Confirm { .. } => modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Dispatch),
            modal::ModalShortcut::enabled("←", confirm_back_action),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
    }
}

fn render_quote_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    service_id: ServiceId,
    quote: &JourneyQuote,
    rejection: Option<&str>,
    service_preselected: bool,
) {
    let insufficient_funds = quote.cash_after_cost < Money::ZERO;
    let status_rows = u16::from(insufficient_funds) + u16::from(rejection.is_some());
    let [
        context_area,
        route_area,
        occupancy_area,
        terms_area,
        status_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(status_rows),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Review Dispatch", theme::title()),
            dispatch_step_line(if service_preselected { 2 } else { 3 }, service_preselected),
        ])
        .style(theme::panel()),
        context_area,
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("TRAIN  ", theme::secondary()),
                Span::styled(
                    format!("Train {:02}", quote.train_id.get()),
                    theme::primary_value(),
                ),
                Span::styled("   SERVICE  ", theme::secondary()),
                Span::styled(
                    format!(
                        "{} · {}",
                        service_name(state, service_id),
                        service_route_label(state, service_id),
                    ),
                    theme::primary_value(),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    train_registration(state, quote.train_id),
                    theme::secondary(),
                ),
                Span::styled(" · ", theme::secondary()),
                Span::styled(
                    train_model_name(state, quote.train_id),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(
                format!(
                    "{} · {} · {}",
                    format_distance(quote.distance.metres()),
                    format_duration(quote.duration.seconds()),
                    format_path(state, quote),
                ),
                theme::secondary(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        route_area,
    );

    let capacity = train_capacity(state, quote.train_id);
    let waiting = waiting_passengers_for_service(state, quote.service_id);
    let occupancy_ratio = if capacity == 0 {
        0.0
    } else {
        f64::from(quote.boarded_passengers.min(capacity)) / f64::from(capacity)
    };
    frame.render_widget(
        Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .title("Passengers")
                    .title_style(theme::title())
                    .style(theme::panel()),
            )
            .gauge_style(theme::focused_title())
            .ratio(occupancy_ratio)
            .label(format!(
                "{} boarded / {} seats · {} waiting",
                quote.boarded_passengers, capacity, waiting
            )),
        occupancy_area,
    );

    let mut terms = vec![Line::styled("PAID AT DEPARTURE", theme::warning())];
    if quote.infrastructure_access_fee_credit > Money::ZERO {
        terms.push(money_pair_line(
            "Access before credit",
            quote.infrastructure_access_fee_before_credit,
            "Access credit",
            quote.infrastructure_access_fee_credit,
        ));
    }
    terms.extend([
        money_pair_line(
            "Access",
            quote.infrastructure_access_fee,
            "Fuel",
            quote.fuel_cost,
        ),
        money_pair_line(
            "Total cost",
            quote.operating_cost,
            "Funds after departure",
            quote.cash_after_cost,
        ),
        Line::styled("BOOKED FROM ORIGIN", theme::success()),
        money_pair_line(
            "Revenue",
            quote.operating_revenue,
            "Quoted result",
            quote.journey_profitability,
        ),
    ]);
    if quote.boarded_passengers == 0 {
        let message = if service_stop_count(state, service_id) > 2 {
            "NO ORIGIN BOARDING · later Service stops may still board passengers."
        } else {
            "EMPTY REPOSITIONING · no passenger revenue is currently booked."
        };
        terms.push(Line::styled(message, theme::warning()));
    }
    if service_stop_count(state, service_id) > 2 {
        terms.push(Line::styled(
            "STOP-BY-STOP · passengers alight and new OD demand may board at each intermediate stop.",
            theme::secondary(),
        ));
    }
    frame.render_widget(
        Paragraph::new(terms)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        terms_area,
    );

    if status_rows > 0 {
        let mut status = Vec::new();
        if insufficient_funds {
            status.push(Line::styled(
                "INSUFFICIENT FUNDS · departure costs exceed current Company Funds.",
                theme::error(),
            ));
        }
        if let Some(rejection) = rejection {
            status.push(Line::styled(rejection, theme::error()));
        }
        frame.render_widget(
            Paragraph::new(status)
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
}

fn money_pair_line(
    first_label: &str,
    first_value: Money,
    second_label: &str,
    second_value: Money,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{first_label}: "), theme::secondary()),
        Span::styled(format_money(first_value), theme::primary_value()),
        Span::styled(format!("  {second_label}: "), theme::secondary()),
        Span::styled(format_money(second_value), theme::primary_value()),
    ])
}

fn dispatch_step_line(active: u8, service_preselected: bool) -> Line<'static> {
    let steps = if service_preselected {
        vec![(1, "TRAIN"), (2, "REVIEW")]
    } else {
        vec![(1, "TRAIN"), (2, "SERVICE"), (3, "REVIEW")]
    };
    let mut spans = Vec::new();
    for (index, (step, label)) in steps.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  →  ", theme::secondary()));
        }
        let style = if *step == active {
            theme::focused_title()
        } else if *step < active {
            theme::success()
        } else {
            theme::secondary()
        };
        spans.push(Span::styled(format!("{step} {label}"), style));
    }
    Line::from(spans)
}

fn train_registration(state: &GameState, train_id: TrainId) -> String {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map(|train| {
            train.evn.marking(
                &state.region.railway_registration.mark,
                &state.player_company.vehicle_keeper_mark,
            )
        })
        .unwrap_or_else(|| "Registration unavailable".into())
}

fn train_model_name(state: &GameState, train_id: TrainId) -> String {
    let Some(train) = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
    else {
        return "Unknown Train".into();
    };
    let model = model_for_train(train)
        .map(|model| model.name().to_owned())
        .unwrap_or_else(|| format!("Unknown model ({})", train.model_id.as_str()));
    train
        .nickname
        .as_ref()
        .map(|nickname| format!("{} · {model}", nickname.as_str()))
        .unwrap_or(model)
}

fn format_signed_money(money: Money) -> String {
    let formatted = format_money(money);
    if money.cents() > 0 {
        format!("+{formatted}")
    } else {
        formatted
    }
}

fn format_money_per_kilometre(cents: u64) -> String {
    format!("${}.{:02}/km", cents / 100, cents % 100)
}

struct TrainChooserContext<'a> {
    state: &'a GameState,
    preferred_station_id: Option<RailStationId>,
    preferred_service_id: Option<ServiceId>,
    selected_train_id: Option<TrainId>,
    rejection: Option<&'a str>,
}

fn render_train_chooser(
    frame: &mut Frame,
    area: Rect,
    chooser: TrainChooserContext<'_>,
    table_state: &mut TableState,
    page_size: &mut usize,
) {
    let state = chooser.state;
    let allowed_train_ids = dispatchable_train_ids_for_context(
        state,
        chooser.preferred_station_id,
        chooser.preferred_service_id,
    );
    let trains = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| allowed_train_ids.contains(&train.id))
        .collect::<Vec<_>>();
    let origin = chooser
        .preferred_station_id
        .map(|station_id| station_label(state, station_id).to_owned());
    let service_context = chooser.preferred_service_id.map(|service_id| {
        format!(
            "{} · {}",
            service_name(state, service_id),
            service_route_label(state, service_id)
        )
    });
    let status_rows = u16::from(chooser.rejection.is_some() || chooser.selected_train_id.is_none());
    let [context_area, body_area, status_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(status_rows),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(vec![
            dispatch_step_line(1, chooser.preferred_service_id.is_some()),
            Line::from(vec![
                Span::styled(
                    if service_context.is_some() {
                        "SERVICE  "
                    } else if origin.is_some() {
                        "FROM  "
                    } else {
                        "READY TRAINS  "
                    },
                    theme::secondary(),
                ),
                Span::styled(
                    service_context.or(origin).unwrap_or_else(|| {
                        "Choose a Train; its current station becomes the origin.".into()
                    }),
                    theme::primary_value(),
                ),
            ]),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    let show_inspector = body_area.width >= 76 && body_area.height >= 7;
    let (list_area, divider_area, inspector_area) = if show_inspector {
        let [list_area, divider_area, inspector_area] = Layout::horizontal([
            Constraint::Min(42),
            Constraint::Length(1),
            Constraint::Length(29),
        ])
        .areas(body_area);
        (list_area, Some(divider_area), Some(inspector_area))
    } else {
        (body_area, None, None)
    };

    let [title_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(list_area);
    frame.render_widget(
        Paragraph::new(Line::styled("Choose Train", theme::title())).style(theme::panel()),
        title_area,
    );

    *page_size = usize::from(table_area.height.saturating_sub(2)).max(1);
    let wide_table = table_area.width >= 62;
    let rows = trains
        .iter()
        .map(|train| {
            let TrainStatus::Ready { at } = train.status else {
                unreachable!("READY Train chooser only includes READY Trains");
            };
            if wide_table {
                Row::new([
                    Cell::from(format!("Train {:02}", train.id.get())),
                    Cell::from(station_label(state, at).to_owned()),
                    Cell::from(train_model_name(state, train.id)),
                    Cell::from(format!("{} pax", train_capacity(state, train.id))),
                ])
            } else {
                Row::new([
                    Cell::from(format!("Train {:02}", train.id.get())),
                    Cell::from(train_model_name(state, train.id)),
                    Cell::from(station_label(state, at).to_owned()),
                ])
            }
        })
        .collect::<Vec<_>>();
    let (headers, widths) = if wide_table {
        (
            Row::new(["Train", "Location", "Model", "Capacity"]),
            vec![
                Constraint::Length(10),
                Constraint::Percentage(25),
                Constraint::Percentage(45),
                Constraint::Length(10),
            ],
        )
    } else {
        (
            Row::new(["Train", "Model", "Location"]),
            vec![
                Constraint::Length(10),
                Constraint::Percentage(52),
                Constraint::Percentage(30),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(headers.style(theme::table_header()).bottom_margin(1))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, table_state);

    if let (Some(divider_area), Some(inspector_area)) = (divider_area, inspector_area) {
        modal::render_vertical_separator(frame, divider_area);
        render_train_inspector(frame, inspector_area, state, chooser.selected_train_id);
    }

    if status_rows > 0 {
        let line = if let Some(rejection) = chooser.rejection {
            Line::styled(rejection, theme::error())
        } else {
            Line::styled("Choose a READY Train to continue.", theme::warning())
        };
        frame.render_widget(
            Paragraph::new(line)
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
}

fn render_train_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_train_id: Option<TrainId>,
) {
    let Some(train_id) = selected_train_id else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Train Preview", theme::title()),
                Line::styled("Select a READY Train to inspect it.", theme::secondary()),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    let Some(train) = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
    else {
        return;
    };
    let location = ready_train_station(state, train_id)
        .map(|station_id| station_label(state, station_id))
        .unwrap_or("Unknown");
    let service_count = service_options(state, train_id).len();
    let registration = train_registration(state, train_id);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Train Preview", theme::title()),
            Line::styled(
                format!("Train {:02}", train.id.get()),
                theme::focused_title(),
            ),
            Line::styled(registration, theme::secondary()),
            Line::styled(train_model_name(state, train_id), theme::primary_value()),
            Line::from(""),
            detail_line("Location", location),
            detail_line(
                "Capacity",
                &format!("{} pax", train_capacity(state, train_id)),
            ),
            detail_line("Speed", &train_speed(state, train_id)),
            detail_line("Fuel", &train_fuel_rate(state, train_id)),
            detail_line("Services", &service_count.to_string()),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

#[derive(Clone, Debug)]
struct ServiceOption {
    service_id: ServiceId,
    quote: JourneyQuote,
}

struct ServiceChooserContext<'a> {
    state: &'a GameState,
    train_id: TrainId,
    rejection: Option<&'a str>,
}

fn render_service_chooser(
    frame: &mut Frame,
    area: Rect,
    chooser: ServiceChooserContext<'_>,
    selected_service_id: &mut Option<ServiceId>,
    table_state: &mut TableState,
    page_size: &mut usize,
) {
    let state = chooser.state;
    let train_id = chooser.train_id;
    let Some(origin_station_id) = ready_train_station(state, train_id) else {
        render_service_unavailable(
            frame,
            area,
            "That Train is no longer READY. Left or Backspace returns to Train selection.",
        );
        return;
    };
    let services = service_options(state, train_id);
    if services.is_empty() {
        render_service_unavailable(
            frame,
            area,
            "No Passenger Service can be operated from this station. Create one from Map → Services, then return to dispatch.",
        );
        return;
    }

    synchronize_service_selection(selected_service_id, table_state, &services);
    let status_rows = u16::from(chooser.rejection.is_some());
    let [context_area, body_area, status_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(status_rows),
    ])
    .areas(area);
    let model = train_model_name(state, train_id);
    let registration = train_registration(state, train_id);
    frame.render_widget(
        Paragraph::new(vec![
            dispatch_step_line(2, false),
            Line::from(vec![
                Span::styled("FROM  ", theme::secondary()),
                Span::styled(
                    station_label(state, origin_station_id),
                    theme::primary_value(),
                ),
                Span::styled("   TRAIN  ", theme::secondary()),
                Span::styled(
                    format!("Train {:02}", train_id.get()),
                    theme::primary_value(),
                ),
            ]),
            Line::from(vec![
                Span::styled(registration, theme::secondary()),
                Span::styled(" · ", theme::secondary()),
                Span::styled(model, theme::primary_value()),
            ]),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    let show_inspector = body_area.width >= 78 && body_area.height >= 7;
    let (list_area, divider_area, inspector_area) = if show_inspector {
        let [list_area, divider_area, inspector_area] = Layout::horizontal([
            Constraint::Min(43),
            Constraint::Length(1),
            Constraint::Length(31),
        ])
        .areas(body_area);
        (list_area, Some(divider_area), Some(inspector_area))
    } else {
        (body_area, None, None)
    };

    let [title_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(list_area);
    frame.render_widget(
        Paragraph::new(Line::styled("Choose Passenger Service", theme::title()))
            .style(theme::panel()),
        title_area,
    );

    *page_size = usize::from(table_area.height.saturating_sub(2)).max(1);
    let wide_table = table_area.width >= 60;
    let rows = services
        .iter()
        .map(|service| {
            let quote = &service.quote;
            let demand = waiting_passengers_for_service(state, quote.service_id);
            if wide_table {
                Row::new([
                    Cell::from(service_name(state, service.service_id)),
                    Cell::from(service_route_label(state, service.service_id)),
                    Cell::from(format!("{demand} waiting")),
                    Cell::from(format_duration(quote.duration.seconds())),
                    Cell::from(format_signed_money(quote.journey_profitability)),
                ])
            } else {
                Row::new([
                    Cell::from(service_name(state, service.service_id)),
                    Cell::from(service_destination_label(state, service.service_id)),
                    Cell::from(format!("{demand} waiting")),
                    Cell::from(format_signed_money(quote.journey_profitability)),
                ])
            }
        })
        .collect::<Vec<_>>();
    let (headers, widths) = if wide_table {
        (
            Row::new(["Service", "Stops", "Demand", "Time", "Est. result"]),
            vec![
                Constraint::Length(10),
                Constraint::Percentage(38),
                Constraint::Percentage(20),
                Constraint::Length(11),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(["Service", "Destination", "Demand", "Est. result"]),
            vec![
                Constraint::Length(10),
                Constraint::Percentage(34),
                Constraint::Percentage(26),
                Constraint::Length(14),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(headers.style(theme::table_header()).bottom_margin(1))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, table_state);

    if let (Some(divider_area), Some(inspector_area)) = (divider_area, inspector_area) {
        modal::render_vertical_separator(frame, divider_area);
        let selected = selected_service_id.and_then(|service_id| {
            services
                .iter()
                .find(|service| service.service_id == service_id)
        });
        render_service_inspector(frame, inspector_area, state, selected);
    }

    if let Some(rejection) = chooser.rejection {
        frame.render_widget(
            Paragraph::new(Line::styled(rejection, theme::error()))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
}

fn render_service_unavailable(frame: &mut Frame, area: Rect, reason: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            dispatch_step_line(2, false),
            Line::styled("Choose Passenger Service", theme::title()),
            Line::from(""),
            Line::styled("Service unavailable", theme::warning()),
            Line::styled(reason, theme::error()),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_service_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    service: Option<&ServiceOption>,
) {
    let Some(service) = service else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Service Preview", theme::title()),
                Line::styled(
                    "Select a Passenger Service to preview its Journey.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    let quote = &service.quote;
    let demand = waiting_passengers_for_service(state, quote.service_id);
    let name = service_name(state, service.service_id);
    let route = service_route_label(state, service.service_id);
    let result_style = if quote.journey_profitability.cents() >= 0 {
        theme::success()
    } else {
        theme::error()
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Service Preview", theme::title()),
            Line::styled(format!("{name} · {route}"), theme::focused_title()),
            Line::styled(
                format!(
                    "{demand} waiting · {}",
                    format_duration(quote.duration.seconds())
                ),
                theme::secondary(),
            ),
            Line::styled(format_distance(quote.distance.metres()), theme::secondary()),
            Line::from(""),
            detail_line("Revenue", &format_money(quote.operating_revenue)),
            detail_line("Cost", &format_money(quote.operating_cost)),
            Line::from(vec![
                Span::styled("Result    ", theme::secondary()),
                Span::styled(
                    format_signed_money(quote.journey_profitability),
                    result_style,
                ),
            ]),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn ready_train_ids(state: &GameState) -> Vec<TrainId> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter_map(|train| match train.status {
            TrainStatus::Ready { .. } => Some(train.id),
            TrainStatus::Travelling { .. } => None,
        })
        .collect()
}

fn ready_train_ids_for_station(
    state: &GameState,
    station_id: Option<RailStationId>,
) -> Vec<TrainId> {
    ready_train_ids(state)
        .into_iter()
        .filter(|train_id| match station_id {
            Some(station_id) => ready_train_station(state, *train_id) == Some(station_id),
            None => true,
        })
        .collect()
}

fn dispatchable_train_ids_for_context(
    state: &GameState,
    station_id: Option<RailStationId>,
    service_id: Option<ServiceId>,
) -> Vec<TrainId> {
    let mut train_ids = ready_train_ids_for_station(state, station_id)
        .into_iter()
        .filter(|train_id| match service_id {
            Some(service_id) => {
                state.player_company.fleet.assigned_service_id(*train_id) == Some(service_id)
                    && preview_quote(state, *train_id, service_id).is_ok()
            }
            None => !service_options(state, *train_id).is_empty(),
        })
        .collect::<Vec<_>>();
    if let Some(service_id) = service_id {
        train_ids.sort_by_key(|train_id| {
            state.player_company.fleet.assigned_service_id(*train_id) != Some(service_id)
        });
    }
    train_ids
}

fn no_ready_train_reason(state: &GameState) -> &'static str {
    if state.player_company.fleet.trains.is_empty() {
        "No READY Train in the Fleet. Open 3 Market to buy a Train."
    } else {
        "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then press D to dispatch."
    }
}

/// Reconciles table focus by stable Train ID. A missing selected Train is
/// deliberately cleared rather than replaced with a different Fleet entry.
fn synchronize_train_selection(
    selected_train_id: &mut Option<TrainId>,
    table_state: &mut TableState,
    train_ids: &[TrainId],
) -> bool {
    let Some(train_id) = *selected_train_id else {
        table_state.select(None);
        return false;
    };
    if let Some(index) = train_ids
        .iter()
        .position(|candidate| *candidate == train_id)
    {
        table_state.select(Some(index));
        false
    } else {
        *selected_train_id = None;
        table_state.select(None);
        *table_state.offset_mut() = 0;
        true
    }
}

fn move_train_selection(
    selected_train_id: &mut Option<TrainId>,
    table_state: &mut TableState,
    train_ids: &[TrainId],
    page_size: usize,
    key: KeyCode,
) {
    let current = selected_train_id.and_then(|train_id| {
        train_ids
            .iter()
            .position(|candidate| *candidate == train_id)
    });
    let last = train_ids.len().saturating_sub(1);
    let next = match key {
        KeyCode::Up | KeyCode::Char('k') => current.map_or(0, |index| index.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            current.map_or(0, |index| index.saturating_add(1).min(last))
        }
        KeyCode::PageUp => current.map_or(0, |index| index.saturating_sub(page_size.max(1))),
        KeyCode::PageDown => {
            current.map_or(0, |index| index.saturating_add(page_size.max(1)).min(last))
        }
        _ => return,
    };
    *selected_train_id = train_ids.get(next).copied();
    table_state.select(Some(next));
}

fn ready_train_station(state: &GameState, train_id: TrainId) -> Option<RailStationId> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(|train| match train.status {
            TrainStatus::Ready { at } => Some(at),
            TrainStatus::Travelling { .. } => None,
        })
}

fn train_capacity(state: &GameState, train_id: TrainId) -> u32 {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(model_for_train)
        .map_or(0, |model| model.passenger_capacity().passengers())
}

fn train_speed(state: &GameState, train_id: TrainId) -> String {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(model_for_train)
        .map(|model| crate::ui::format::speed_kmh(model.speed().metres_per_second()))
        .unwrap_or_else(|| "Unavailable".into())
}

fn train_fuel_rate(state: &GameState, train_id: TrainId) -> String {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(model_for_train)
        .map(|model| {
            format_money_per_kilometre(model.fuel_cost_per_kilometre().cents_per_kilometre())
        })
        .unwrap_or_else(|| "Unavailable".into())
}

fn train_selection_step(
    state: &GameState,
    selected_train_id: Option<TrainId>,
    preferred_station_id: Option<RailStationId>,
    preferred_service_id: Option<ServiceId>,
) -> DispatchStep {
    let train_ids =
        dispatchable_train_ids_for_context(state, preferred_station_id, preferred_service_id);
    let selected = selected_train_id.and_then(|train_id| {
        train_ids
            .iter()
            .position(|candidate| *candidate == train_id)
    });
    let mut table_state = TableState::default();
    table_state.select(selected);
    DispatchStep::SelectTrain {
        selected_train_id: selected.map(|index| train_ids[index]),
        table_state,
        page_size: 1,
    }
}

fn service_step(
    state: &GameState,
    train_id: TrainId,
    selected_service_id: Option<ServiceId>,
) -> Result<DispatchStep, String> {
    let services = service_options(state, train_id);
    if services.is_empty() {
        return Err(if state
            .player_company
            .fleet
            .assigned_service_id(train_id)
            .is_none()
        {
            "Assign a Passenger Service to this Train before revenue dispatch.".into()
        } else {
            "This Train's assigned Passenger Service cannot be operated from its current Rail Station."
                .into()
        });
    }
    let selected = selected_service_id
        .and_then(|service_id| {
            services
                .iter()
                .position(|service| service.service_id == service_id)
        })
        .unwrap_or(0);
    let mut table_state = TableState::default();
    table_state.select(Some(selected));
    Ok(DispatchStep::SelectService {
        train_id,
        selected_service_id: Some(services[selected].service_id),
        table_state,
        page_size: 1,
    })
}

fn service_options(state: &GameState, train_id: TrainId) -> Vec<ServiceOption> {
    if ready_train_station(state, train_id).is_none() {
        return Vec::new();
    }
    let Some(assigned_service_id) = state.player_company.fleet.assigned_service_id(train_id) else {
        return Vec::new();
    };
    state
        .player_company
        .passenger_services
        .iter()
        .filter(|service| service.id == assigned_service_id)
        .filter_map(|service| {
            preview_quote(state, train_id, service.id)
                .ok()
                .map(|quote| ServiceOption {
                    service_id: service.id,
                    quote,
                })
        })
        .collect()
}

fn preview_quote(
    state: &GameState,
    train_id: TrainId,
    service_id: ServiceId,
) -> Result<JourneyQuote, String> {
    quote_journey(state, train_id, service_id).map_err(|error| error.to_string())
}

fn synchronize_service_selection(
    selected_service_id: &mut Option<ServiceId>,
    table_state: &mut TableState,
    services: &[ServiceOption],
) -> bool {
    let Some(service_id) = *selected_service_id else {
        table_state.select(None);
        return false;
    };
    if let Some(index) = services
        .iter()
        .position(|service| service.service_id == service_id)
    {
        table_state.select(Some(index));
        false
    } else {
        *selected_service_id = None;
        table_state.select(None);
        *table_state.offset_mut() = 0;
        true
    }
}

fn move_service_selection(
    selected_service_id: &mut Option<ServiceId>,
    table_state: &mut TableState,
    services: &[ServiceOption],
    page_size: usize,
    key: KeyCode,
) {
    let current = selected_service_id.and_then(|service_id| {
        services
            .iter()
            .position(|service| service.service_id == service_id)
    });
    let last = services.len().saturating_sub(1);
    let next = match key {
        KeyCode::Up | KeyCode::Char('k') => current.map_or(0, |index| index.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            current.map_or(0, |index| index.saturating_add(1).min(last))
        }
        KeyCode::PageUp => current.map_or(0, |index| index.saturating_sub(page_size.max(1))),
        KeyCode::PageDown => {
            current.map_or(0, |index| index.saturating_add(page_size.max(1)).min(last))
        }
        _ => return,
    };
    *selected_service_id = services.get(next).map(|service| service.service_id);
    table_state.select(Some(next));
}

fn service_name(state: &GameState, service_id: ServiceId) -> String {
    state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .map(|service| service.display_name())
        .unwrap_or_else(|| format!("Service {}", service_id.get()))
}

fn service_route_label(state: &GameState, service_id: ServiceId) -> String {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return "Unknown route".into();
    };
    service
        .stop_station_ids
        .iter()
        .map(|station_id| station_label(state, *station_id).to_owned())
        .collect::<Vec<_>>()
        .join(" → ")
}

fn service_stop_count(state: &GameState, service_id: ServiceId) -> usize {
    state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .map_or(0, |service| service.stop_station_ids.len())
}

fn service_destination_label(state: &GameState, service_id: ServiceId) -> String {
    state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .and_then(|service| service.destination_station_id())
        .map(|station_id| station_label(state, station_id).to_owned())
        .unwrap_or_else(|| "Unknown".into())
}

fn station_label(state: &GameState, station_id: RailStationId) -> &str {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return "unknown Rail Station";
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map_or("unknown Settlement", |settlement| settlement.name.as_str())
}

fn waiting_passengers_for_service(state: &GameState, service_id: ServiceId) -> u32 {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return 0;
    };
    let Some(origin_station_id) = service.origin_station_id() else {
        return 0;
    };
    service
        .stop_station_ids
        .iter()
        .skip(1)
        .filter_map(|destination_station_id| {
            state.origin_destination_demand.iter().find(|demand| {
                demand.origin_station_id == origin_station_id
                    && demand.destination_station_id == *destination_station_id
            })
        })
        .fold(0_u32, |total, demand| {
            total.saturating_add(demand.waiting_passengers)
        })
}

fn format_path(state: &GameState, quote: &JourneyQuote) -> String {
    let mut stations = vec![quote.origin_station_id];
    let mut current_station_id = quote.origin_station_id;
    for rail_line_id in &quote.rail_line_path {
        let Some(rail_line) = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .find(|rail_line| rail_line.id == *rail_line_id)
        else {
            return "unknown Rail Line path".into();
        };
        let next_station_id = if rail_line.first_station_id == current_station_id {
            rail_line.second_station_id
        } else if rail_line.second_station_id == current_station_id {
            rail_line.first_station_id
        } else {
            return "invalid Rail Line path".into();
        };
        stations.push(next_station_id);
        current_station_id = next_station_id;
    }
    stations
        .into_iter()
        .map(|station_id| station_label(state, station_id))
        .collect::<Vec<_>>()
        .join(" → ")
}

fn format_distance(metres: u64) -> String {
    crate::ui::format::distance(metres)
}

fn format_duration(seconds: u64) -> String {
    crate::ui::format::duration(seconds)
}

fn format_money(money: Money) -> String {
    crate::ui::format::money(money)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{RailStationId, TrainStatus, UtcSeconds},
        sim::{
            fleet::purchase_train,
            services::{assign_train_to_service, create_service},
            world::create_new_game,
        },
    };

    use super::{DispatchFlow, DispatchFlowAction};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn game_with_ready_train(
        at: RailStationId,
    ) -> (crate::model::GameState, crate::model::TrainId) {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, at).unwrap();
        (state, train_id)
    }

    fn add_service(
        state: &mut crate::model::GameState,
        stops: Vec<RailStationId>,
    ) -> crate::model::ServiceId {
        let service_id = create_service(state, stops).unwrap();
        let train_id = state.player_company.fleet.trains[0].id;
        assign_train_to_service(state, train_id, service_id).unwrap();
        service_id
    }


    #[test]
    fn selected_unassigned_train_cannot_start_revenue_dispatch() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(1));
        create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        )
        .unwrap();

        assert_eq!(
            DispatchFlow::start_with_selected_train(&state, train_id),
            Err(format!(
                "Train {} is UNASSIGNED. Assign a Passenger Service from Fleet or Services before revenue dispatch.",
                train_id.get()
            ))
        );
        assert_eq!(
            DispatchFlow::start(&state),
            Err(
                "No READY Train is assigned to a Passenger Service. Assign one from Fleet or Services before revenue dispatch."
            )
        );
    }

    #[test]
    fn service_preselected_dispatch_requires_an_assigned_train() {
        let (mut state, _) = game_with_ready_train(RailStationId::new(1));
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        )
        .unwrap();

        assert_eq!(
            DispatchFlow::start_with_selected_service(&state, service_id),
            Err("No assigned READY Train can run R1 from a valid terminus.".into())
        );
    }

    #[test]
    fn station_scoped_dispatch_uses_the_same_train_first_workflow() {
        let (mut state, _) = game_with_ready_train(RailStationId::new(1));
        add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let flow = DispatchFlow::start_at_station(&state, Some(RailStationId::new(1))).unwrap();

        assert!(flow.is_selecting_train());
        assert!(!flow.is_selecting_service());
    }

    #[test]
    fn global_dispatch_still_lists_the_only_ready_train() {
        let (mut state, _) = game_with_ready_train(RailStationId::new(1));
        add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let flow = DispatchFlow::start(&state).unwrap();

        assert!(flow.is_selecting_train());
        assert!(!flow.is_selecting_service());
    }

    #[test]
    fn existing_multi_stop_service_is_keyboard_reachable() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(1));
        let service_id = add_service(
            &mut state,
            vec![
                RailStationId::new(1),
                RailStationId::new(2),
                RailStationId::new(3),
            ],
        );
        let mut flow = DispatchFlow::start(&state).unwrap();

        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert!(flow.is_selecting_service());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );

        let rendered = flow.render(&state);
        assert!(rendered.contains("Passenger Service"));
        assert!(rendered.contains("R1"));
        assert!(rendered.contains("Directional Demand:"));
        assert!(rendered.contains("Waiting Passengers"));
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                service_id,
            }
        );
    }

    #[test]
    fn bidirectional_service_is_dispatchable_from_reverse_terminus() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(3));
        let service_id = add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let mut flow = DispatchFlow::start(&state).unwrap();

        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert!(flow.is_selecting_service());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert!(flow.is_confirming());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                service_id,
            }
        );
    }

    #[test]
    fn service_preselected_dispatch_skips_service_picker() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(3));
        let service_id = add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let mut flow = DispatchFlow::start_with_selected_service(&state, service_id).unwrap();

        assert!(flow.is_selecting_train());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert!(flow.is_confirming());
        assert!(!flow.is_selecting_service());

        assert_eq!(
            flow.handle_key(key(KeyCode::Left), &state),
            DispatchFlowAction::Continue
        );
        assert!(flow.is_selecting_train());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                service_id,
            }
        );
    }

    #[test]
    fn cancel_leaves_every_domain_collection_unchanged() {
        let (mut state, _) = game_with_ready_train(RailStationId::new(1));
        add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let before = state.clone();
        let mut flow = DispatchFlow::start(&state).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            flow.handle_key(key(KeyCode::Esc), &state),
            DispatchFlowAction::Cancel
        );

        assert_eq!(state, before);
    }

    #[test]
    fn rejection_stays_visible_with_its_concrete_cause() {
        let (mut state, _) = game_with_ready_train(RailStationId::new(1));
        add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        let mut flow = DispatchFlow::start(&state).unwrap();
        flow.reject("Company Funds of 1 cents cannot cover Journey departure costs of 2 cents");

        assert!(
            flow.render(&state)
                .contains("Departure rejected: Company Funds")
        );
    }

    #[test]
    fn preview_does_not_treat_a_travelling_train_as_ready() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(1));
        add_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(3)],
        );
        state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
            journey_id: crate::model::JourneyId::new(1),
        };
        assert_eq!(
            DispatchFlow::start(&state),
            Err(
                "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then press D to dispatch."
            )
        );
        assert_eq!(train_id.get(), 1);
    }
}

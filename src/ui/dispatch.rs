//! Keyboard state and text presentation for a Map Manual Dispatch.
//!
//! This module only keeps a proposed Train and destination. It previews a
//! Passenger Service on a cloned state, so the actual Service is not created
//! until the application boundary confirms the Manual Dispatch.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{GameState, Money, RailStationId, TrainId, TrainStatus},
    sim::{
        economy::{JourneyQuote, quote_journey},
        services::find_or_create_service,
    },
    ui::theme,
};

/// The result of handling a key within the Map Manual Dispatch flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchFlowAction {
    /// The player is still reviewing or selecting the dispatch.
    Continue,
    /// The player abandoned the flow without authorising a Journey.
    Cancel,
    /// The application boundary must revalidate and authorise this dispatch.
    Confirm {
        train_id: TrainId,
        destination_station_id: RailStationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DispatchStep {
    SelectTrain {
        selected_train_id: Option<TrainId>,
        table_state: TableState,
        page_size: usize,
    },
    SelectDestination {
        train_id: TrainId,
        selected: usize,
    },
    Confirm {
        train_id: TrainId,
        destination_station_id: RailStationId,
        quote: JourneyQuote,
        reuses_service: bool,
    },
}

/// Presentation state for one uncommitted Manual Dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchFlow {
    step: DispatchStep,
    preferred_station_id: Option<RailStationId>,
    rejection: Option<String>,
}

impl DispatchFlow {
    /// Starts selecting a READY Train. No game state changes at this point.
    pub fn start(state: &GameState) -> Result<Self, &'static str> {
        Self::start_at_station(state, None)
    }

    /// Starts at a focused Rail Station, preferring one of its READY Trains.
    /// The player can still deliberately choose any other available Fleet Train.
    pub fn start_at_station(
        state: &GameState,
        preferred_station_id: Option<RailStationId>,
    ) -> Result<Self, &'static str> {
        let train_ids = ready_train_ids(state);
        if train_ids.is_empty() {
            return Err(no_ready_train_reason(state));
        }
        let selected = preferred_station_id
            .and_then(|station_id| {
                train_ids
                    .iter()
                    .position(|train_id| ready_train_station(state, *train_id) == Some(station_id))
            })
            .unwrap_or(0);
        let mut table_state = TableState::default();
        table_state.select(Some(selected));
        Ok(Self {
            step: DispatchStep::SelectTrain {
                selected_train_id: Some(train_ids[selected]),
                table_state,
                page_size: 1,
            },
            preferred_station_id,
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
                let trains = ready_train_ids(state);
                if trains.is_empty() {
                    self.rejection = Some(no_ready_train_reason(state).into());
                    return DispatchFlowAction::Continue;
                }
                if synchronize_train_selection(selected_train_id, table_state, &trains) {
                    self.rejection = Some(
                        "The previously selected Train is no longer READY; choose an available Train."
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
                    let Some(origin_station_id) = ready_train_station(state, train_id) else {
                        self.rejection =
                            Some("That Train is no longer READY; select a Train again.".into());
                        return DispatchFlowAction::Continue;
                    };
                    if destination_station_ids(state, origin_station_id).is_empty() {
                        self.rejection =
                            Some("No other connected Rail Station is available.".into());
                        return DispatchFlowAction::Continue;
                    }
                    self.step = DispatchStep::SelectDestination {
                        train_id,
                        selected: 0,
                    };
                    self.rejection = None;
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::SelectDestination { train_id, selected } => {
                let Some(origin_station_id) = ready_train_station(state, *train_id) else {
                    self.rejection =
                        Some("That Train is no longer READY; select a Train again.".into());
                    return DispatchFlowAction::Continue;
                };
                let destinations = destination_station_ids(state, origin_station_id);
                if destinations.is_empty() {
                    self.rejection = Some("No other connected Rail Station is available.".into());
                    return DispatchFlowAction::Continue;
                }
                move_selection(selected, destinations.len(), key.code);
                if matches!(key.code, KeyCode::Enter) {
                    let destination_station_id = destinations[*selected];
                    match preview_quote(state, *train_id, destination_station_id) {
                        Ok((quote, reuses_service)) => {
                            self.step = DispatchStep::Confirm {
                                train_id: *train_id,
                                destination_station_id,
                                quote,
                                reuses_service,
                            };
                            self.rejection = None;
                        }
                        Err(error) => self.rejection = Some(error),
                    }
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::Confirm {
                train_id,
                destination_station_id,
                ..
            } => {
                if matches!(key.code, KeyCode::Enter) {
                    DispatchFlowAction::Confirm {
                        train_id: *train_id,
                        destination_station_id: *destination_station_id,
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
                for train_id in ready_train_ids(state) {
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
                        train.model_name
                    ));
                }
            }
            DispatchStep::SelectDestination { train_id, selected } => {
                let origin = ready_train_station(state, *train_id);
                output.push_str(&format!(
                    "Select a destination for Train {} from {} (Up/Down, Enter; Esc cancels):\n",
                    train_id.get(),
                    origin
                        .map(|station_id| station_label(state, station_id))
                        .unwrap_or("unknown Rail Station")
                ));
                if let Some(origin_station_id) = origin {
                    for (index, station_id) in destination_station_ids(state, origin_station_id)
                        .iter()
                        .enumerate()
                    {
                        let marker = if index == *selected { '>' } else { ' ' };
                        output.push_str(&format!(
                            " {marker} {}\n",
                            station_label(state, *station_id)
                        ));
                    }
                }
            }
            DispatchStep::Confirm {
                quote,
                reuses_service,
                ..
            } => {
                output.push_str(&format!(
                    "{} Passenger Service: {} -> {} ({} Rail Lines)\n",
                    if *reuses_service { "Reuse" } else { "Create" },
                    station_label(state, quote.origin_station_id),
                    station_label(state, quote.destination_station_id),
                    quote.rail_line_path.len(),
                ));
                output.push_str(&format!(
                    "Directional Demand: {} Waiting Passengers; {} boarding\n",
                    waiting_passengers(
                        state,
                        quote.origin_station_id,
                        quote.destination_station_id
                    ),
                    quote.boarded_passengers,
                ));
                output.push_str(&format!(
                    "Distance: {} m | Fare: {} each | Revenue on arrival: {}\n",
                    quote.distance.metres(),
                    format_money(quote.fare),
                    format_money(quote.operating_revenue),
                ));
                output.push_str(&format!(
                    "Infrastructure Access Fee: {} | Fuel Cost: {} | Departure cost: {}\n",
                    format_money(quote.infrastructure_access_fee),
                    format_money(quote.fuel_cost),
                    format_money(quote.operating_cost),
                ));
                output.push_str(&format!(
                    "Journey Profitability: {} | Duration: {}s | Company Funds after departure: {}\n",
                    format_money(quote.journey_profitability),
                    quote.duration.seconds(),
                    format_money(quote.cash_after_cost),
                ));
                output.push_str("Enter confirms Manual Dispatch (revalidated); Esc cancels.\n");
            }
        }
        if let Some(rejection) = &self.rejection {
            output.push_str(&format!("Departure rejected: {rejection}\n"));
        }
        output
    }

    /// Renders the stateful READY Train chooser and leaves later flow steps on
    /// their existing presentation until their dedicated flow cards replace them.
    pub fn render_panel(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        let selection_changed = match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                ..
            } => {
                synchronize_train_selection(selected_train_id, table_state, &ready_train_ids(state))
            }
            DispatchStep::SelectDestination { .. } | DispatchStep::Confirm { .. } => false,
        };
        if selection_changed {
            self.rejection = Some(
                "The previously selected Train is no longer READY; choose an available Train."
                    .into(),
            );
        }

        match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                page_size,
            } => render_train_chooser(
                frame,
                area,
                TrainChooserContext {
                    state,
                    preferred_station_id: self.preferred_station_id,
                    selected_train_id: *selected_train_id,
                    rejection: self.rejection.as_deref(),
                },
                table_state,
                page_size,
            ),
            DispatchStep::SelectDestination { .. } | DispatchStep::Confirm { .. } => {
                frame.render_widget(
                    Paragraph::new(self.render(state))
                        .block(dispatch_panel_block("Manual Dispatch", true))
                        .style(theme::panel())
                        .wrap(Wrap { trim: false }),
                    area,
                );
            }
        }
    }
}

struct TrainChooserContext<'a> {
    state: &'a GameState,
    preferred_station_id: Option<RailStationId>,
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
    let trains = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .collect::<Vec<_>>();
    let station_context = chooser.preferred_station_id.map_or_else(
        || "Choose a READY Train from the available Fleet.".to_owned(),
        |station_id| {
            let station = station_label(state, station_id);
            if trains
                .iter()
                .any(|train| matches!(train.status, TrainStatus::Ready { at } if at == station_id))
            {
                format!("{station}: a READY Train here is preselected.")
            } else {
                format!("No READY Train at {station}; showing the available Fleet.")
            }
        },
    );
    let footer_rows = u16::from(chooser.rejection.is_some() || chooser.selected_train_id.is_none())
        .saturating_add(1);
    let [context_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("1 Train", theme::focused_title()),
            Span::styled(" → 2 Destination → 3 Review", theme::secondary()),
            Span::styled(format!("  {station_context}"), theme::secondary()),
        ]))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    *page_size = usize::from(table_area.height.saturating_sub(4)).max(1);
    let rows = trains
        .iter()
        .map(|train| {
            let TrainStatus::Ready { at } = train.status else {
                unreachable!("READY Train chooser only includes READY Trains");
            };
            Row::new([
                Cell::from(format!("Train {:02}", train.id.get())),
                Cell::from(train.model_name.clone()),
                Cell::from("READY"),
                Cell::from(format!("At {}", station_label(state, at))),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(36),
            Constraint::Length(9),
            Constraint::Percentage(44),
        ],
    )
    .header(
        Row::new(["Train", "Model", "Status", "Location"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .block(dispatch_panel_block(
        "Manual Dispatch · available Fleet",
        true,
    ))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("> ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, table_state);

    let controls = if area.width <= 80 {
        "↑↓ / J K · select   PgUp/Dn · scroll   Enter · next   Esc · cancel"
    } else {
        "↑↓ / J K · select   PageUp / PageDown · scroll   Enter · destination   Esc · cancel"
    };
    let mut footer = vec![Line::styled(controls, theme::hint())];
    if let Some(rejection) = chooser.rejection {
        footer.insert(
            0,
            Line::styled(format!("Dispatch unavailable: {rejection}"), theme::error()),
        );
    }
    if chooser.selected_train_id.is_none() && chooser.rejection.is_none() {
        footer.insert(
            0,
            Line::styled("Choose a READY Train to continue.", theme::warning()),
        );
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn dispatch_panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            theme::focused_border()
        } else {
            theme::border()
        })
        .title(title)
        .title_style(if focused {
            theme::focused_title()
        } else {
            theme::title()
        })
        .style(theme::panel())
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

fn no_ready_train_reason(state: &GameState) -> &'static str {
    if state.player_company.fleet.trains.is_empty() {
        "No READY Train in the Fleet. Press B to buy a Train."
    } else {
        "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then dispatch from Map."
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

fn destination_station_ids(
    state: &GameState,
    origin_station_id: RailStationId,
) -> Vec<RailStationId> {
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .filter(|station_id| *station_id != origin_station_id)
        .collect()
}

fn preview_quote(
    state: &GameState,
    train_id: TrainId,
    destination_station_id: RailStationId,
) -> Result<(JourneyQuote, bool), String> {
    let origin_station_id = ready_train_station(state, train_id)
        .ok_or_else(|| "That Train is no longer READY; select a Train again.".to_owned())?;
    let reuses_service = state
        .player_company
        .passenger_services
        .iter()
        .any(|service| {
            (service.first_station_id == origin_station_id
                && service.second_station_id == destination_station_id)
                || (service.first_station_id == destination_station_id
                    && service.second_station_id == origin_station_id)
        });
    let mut preview_state = state.clone();
    let service_id = find_or_create_service(
        &mut preview_state,
        origin_station_id,
        destination_station_id,
    )
    .map_err(|error| error.to_string())?;
    quote_journey(&preview_state, train_id, service_id)
        .map_err(|error| error.to_string())
        .map(|quote| (quote, reuses_service))
}

fn move_selection(selected: &mut usize, length: usize, key: KeyCode) {
    match key {
        KeyCode::Up | KeyCode::Char('k') if *selected > 0 => *selected -= 1,
        KeyCode::Down | KeyCode::Char('j') if *selected + 1 < length => *selected += 1,
        _ => {}
    }
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

fn waiting_passengers(
    state: &GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> u32 {
    state
        .origin_destination_demand
        .iter()
        .find(|demand| {
            demand.origin_station_id == origin_station_id
                && demand.destination_station_id == destination_station_id
        })
        .map_or(0, |demand| demand.waiting_passengers)
}

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{RailStationId, TrainStatus, UtcSeconds},
        sim::{fleet::purchase_train, services::find_or_create_service, world::create_new_game},
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

    #[test]
    fn multi_line_trip_is_keyboard_reachable_without_creating_a_service() {
        let (state, train_id) = game_with_ready_train(RailStationId::new(1));
        let mut flow = DispatchFlow::start(&state).unwrap();

        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Down), &state),
            DispatchFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );

        let rendered = flow.render(&state);
        assert!(rendered.contains("2 Rail Lines"));
        assert!(rendered.contains("Directional Demand:"));
        assert!(rendered.contains("Waiting Passengers"));
        assert!(state.player_company.passenger_services.is_empty());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                destination_station_id: RailStationId::new(3),
            }
        );
    }

    #[test]
    fn return_trip_is_keyboard_reachable_and_reuses_the_service_preview() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(3));
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(3)).unwrap();
        let mut flow = DispatchFlow::start(&state).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);

        let rendered = flow.render(&state);
        assert!(rendered.contains("Reuse Passenger Service"));
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                destination_station_id: RailStationId::new(1),
            }
        );
    }

    #[test]
    fn cancel_leaves_every_domain_collection_unchanged() {
        let (state, _) = game_with_ready_train(RailStationId::new(1));
        let before = state.clone();
        let mut flow = DispatchFlow::start(&state).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Down), &state);
        flow.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            flow.handle_key(key(KeyCode::Esc), &state),
            DispatchFlowAction::Cancel
        );

        assert_eq!(state, before);
    }

    #[test]
    fn rejection_stays_visible_with_its_concrete_cause() {
        let (state, _) = game_with_ready_train(RailStationId::new(1));
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
        state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
            journey_id: crate::model::JourneyId::new(1),
        };
        assert_eq!(
            DispatchFlow::start(&state),
            Err(
                "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then dispatch from Map."
            )
        );
        assert_eq!(train_id.get(), 1);
    }
}

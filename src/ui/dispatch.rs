//! Keyboard state and text presentation for a Map Manual Dispatch.
//!
//! This module only keeps a proposed Train and destination. It previews a
//! Passenger Service on a cloned state, so the actual Service is not created
//! until the application boundary confirms the Manual Dispatch.

use crossterm::event::{KeyCode, KeyEvent};

use crate::{
    model::{GameState, Money, RailStationId, TrainId, TrainStatus},
    sim::{
        economy::{JourneyQuote, quote_journey},
        services::find_or_create_service,
    },
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
        selected: usize,
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
    rejection: Option<String>,
}

impl DispatchFlow {
    /// Starts selecting a READY Train. No game state changes at this point.
    pub fn start(state: &GameState) -> Result<Self, &'static str> {
        if ready_train_ids(state).is_empty() {
            return Err("No READY Train is available for a Manual Dispatch.");
        }
        Ok(Self {
            step: DispatchStep::SelectTrain { selected: 0 },
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> DispatchFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return DispatchFlowAction::Cancel;
        }

        match &mut self.step {
            DispatchStep::SelectTrain { selected } => {
                let trains = ready_train_ids(state);
                if trains.is_empty() {
                    self.rejection =
                        Some("No READY Train is available for a Manual Dispatch.".into());
                    return DispatchFlowAction::Continue;
                }
                move_selection(selected, trains.len(), key.code);
                if matches!(key.code, KeyCode::Enter) {
                    let train_id = trains[*selected];
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
            DispatchStep::SelectTrain { selected } => {
                output.push_str("Select a READY Train (Up/Down, Enter; Esc cancels):\n");
                for (index, train_id) in ready_train_ids(state).iter().enumerate() {
                    let marker = if index == *selected { '>' } else { ' ' };
                    let Some(train) = state
                        .player_company
                        .fleet
                        .trains
                        .iter()
                        .find(|train| train.id == *train_id)
                    else {
                        continue;
                    };
                    let location = ready_train_station(state, *train_id)
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
            Err("No READY Train is available for a Manual Dispatch.")
        );
        assert_eq!(train_id.get(), 1);
    }
}

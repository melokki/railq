//! Fleet presentation and the confirmed Train resale flow.
//!
//! A resale proposal is kept outside the simulation until the Player Company
//! explicitly confirms it. The application boundary then advances time,
//! revalidates that the Train is READY, and persists the sale.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};

use crate::model::{GameState, Journey, Money, RailStationId, TrainId, TrainStatus, UtcSeconds};

/// The result of handling a key within the resale flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FleetFlowAction {
    /// The player is still reviewing a possible resale.
    Continue,
    /// The player abandoned the resale without changing the Fleet.
    Cancel,
    /// The application boundary must revalidate and sell the selected Train.
    Confirm { train_id: TrainId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FleetStep {
    SelectTrain { selected: usize },
    Confirm { train_id: TrainId },
}

/// Presentation state for one uncommitted Train resale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetFlow {
    step: FleetStep,
    rejection: Option<String>,
}

impl FleetFlow {
    /// Starts selecting a READY Train for resale without changing the Fleet.
    pub fn start(state: &GameState) -> Result<Self, &'static str> {
        if ready_train_ids(state).is_empty() {
            return Err(
                "No READY Train is available for resale. Travelling Trains must arrive first.",
            );
        }
        Ok(Self {
            step: FleetStep::SelectTrain { selected: 0 },
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> FleetFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return FleetFlowAction::Cancel;
        }

        match &mut self.step {
            FleetStep::SelectTrain { selected } => {
                let trains = ready_train_ids(state);
                if trains.is_empty() {
                    self.rejection = Some(
                        "No READY Train is available for resale. Travelling Trains must arrive first."
                            .into(),
                    );
                    return FleetFlowAction::Continue;
                }
                move_selection(selected, trains.len(), key.code);
                if matches!(key.code, KeyCode::Enter) {
                    self.step = FleetStep::Confirm {
                        train_id: trains[*selected],
                    };
                    self.rejection = None;
                }
                FleetFlowAction::Continue
            }
            FleetStep::Confirm { train_id } if matches!(key.code, KeyCode::Enter) => {
                FleetFlowAction::Confirm {
                    train_id: *train_id,
                }
            }
            FleetStep::Confirm { .. } => FleetFlowAction::Continue,
        }
    }

    /// Records an application-boundary rejection while keeping the proposal visible.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the current proposal and its confirmation instructions.
    pub fn render(&self, state: &GameState, now: UtcSeconds) -> String {
        let mut output = render_at(state, now);
        match &self.step {
            FleetStep::SelectTrain { selected } => {
                writeln!(
                    output,
                    "\nSelect a READY Train to sell (Up/Down, Enter; Esc cancels):"
                )
                .expect("writing to a String cannot fail");
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
                    writeln!(
                        output,
                        " {marker} Train {} ({}) — sale proceeds {}",
                        train.id.get(),
                        train.model_name,
                        format_money(resale_proceeds(train.original_purchase_price)),
                    )
                    .expect("writing to a String cannot fail");
                }
            }
            FleetStep::Confirm { train_id } => {
                if let Some(train) = state
                    .player_company
                    .fleet
                    .trains
                    .iter()
                    .find(|train| train.id == *train_id)
                {
                    writeln!(
                        output,
                        "\nSell Train {} ({}) for {} sale proceeds?",
                        train.id.get(),
                        train.model_name,
                        format_money(resale_proceeds(train.original_purchase_price)),
                    )
                    .expect("writing to a String cannot fail");
                } else {
                    writeln!(output, "\nThat Train is no longer in the Fleet.")
                        .expect("writing to a String cannot fail");
                }
                writeln!(output, "Enter confirms resale (revalidated); Esc cancels.")
                    .expect("writing to a String cannot fail");
            }
        }
        if let Some(rejection) = &self.rejection {
            writeln!(output, "Resale rejected: {rejection}")
                .expect("writing to a String cannot fail");
        }
        output
    }
}

/// Renders the Player Company's Fleet at the supplied time.
pub fn render_at(state: &GameState, now: UtcSeconds) -> String {
    let mut output = String::from("Fleet\n");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");

    if state.player_company.fleet.trains.is_empty() {
        writeln!(
            output,
            "\nNo Trains in the Fleet. Buy a Train to begin operating."
        )
        .expect("writing to a String cannot fail");
        return output;
    }

    for train in &state.player_company.fleet.trains {
        match train.status {
            TrainStatus::Ready { at } => {
                writeln!(
                    output,
                    "\nTrain {} — {}\n  READY at {}\n  Eligible sale proceeds: {} (70% of original purchase price)",
                    train.id.get(),
                    train.model_name,
                    station_label(state, at),
                    format_money(resale_proceeds(train.original_purchase_price)),
                )
                .expect("writing to a String cannot fail");
            }
            TrainStatus::Travelling { journey_id } => {
                let journey = state
                    .active_journeys
                    .iter()
                    .find(|journey| journey.id == journey_id);
                match journey {
                    Some(journey) => writeln!(
                        output,
                        "\nTrain {} — {}\n  TRAVELLING {} -> {} | progress: {}% | ETA: {}\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train.model_name,
                        station_label(state, journey.origin_station_id),
                        station_label(state, journey.destination_station_id),
                        journey_progress_percent(journey, now),
                        format_duration(remaining_seconds(journey, now)),
                    )
                    .expect("writing to a String cannot fail"),
                    None => writeln!(
                        output,
                        "\nTrain {} — {}\n  TRAVELLING on Journey {} (details unavailable)\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train.model_name,
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }
    writeln!(output, "\nEnter starts a resale for a READY Train.")
        .expect("writing to a String cannot fail");
    output
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

fn move_selection(selected: &mut usize, length: usize, key: KeyCode) {
    match key {
        KeyCode::Up | KeyCode::Char('k') if *selected > 0 => *selected -= 1,
        KeyCode::Down | KeyCode::Char('j') if *selected + 1 < length => *selected += 1,
        _ => {}
    }
}

fn resale_proceeds(original_purchase_price: Money) -> Money {
    original_purchase_price
        .cents()
        .checked_mul(70)
        .map(|cents| Money::from_cents(cents / 100))
        .unwrap_or(Money::ZERO)
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

fn journey_progress_percent(journey: &Journey, now: UtcSeconds) -> u64 {
    let duration = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds());
    if duration <= 0 {
        return 100;
    }
    let elapsed = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, duration);
    u64::try_from(elapsed.saturating_mul(100) / duration).unwrap_or(100)
}

fn remaining_seconds(journey: &Journey, now: UtcSeconds) -> u64 {
    u64::try_from(
        journey
            .arrives_at
            .unix_seconds()
            .saturating_sub(now.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
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
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::{purchase_train, sell_train},
            journeys::dispatch_journey,
            services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{FleetFlow, FleetFlowAction, render_at};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn fleet_shows_ready_location_and_confirmed_resale_proceeds() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let rendered = render_at(&state, STARTED_AT);

        let station = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .find(|station| station.id == RailStationId::new(1))
            .unwrap();
        let settlement = state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == station.settlement_id)
            .unwrap();
        assert!(rendered.contains(&format!("READY at {}", settlement.name)));
        assert!(rendered.contains("Eligible sale proceeds:"));

        let mut flow = FleetFlow::start(&state).unwrap();
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            FleetFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            FleetFlowAction::Confirm { train_id }
        );
        assert!(flow.render(&state, STARTED_AT).contains("sale proceeds"));
        assert!(sell_train(&mut state, train_id).is_ok());
    }

    #[test]
    fn travelling_train_shows_progress_eta_and_cannot_enter_resale_flow() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(2);
        let train_id = purchase_train(&mut state, 0, origin).unwrap();
        let service_id = find_or_create_service(&mut state, origin, destination).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let journey = &state.active_journeys[0];
        let halfway = UtcSeconds::from_unix_seconds(
            journey.departed_at.unix_seconds()
                + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
        );

        let rendered = render_at(&state, halfway);
        assert!(rendered.contains("TRAVELLING"));
        assert!(rendered.contains("progress: 50%"));
        assert!(rendered.contains("ETA:"));
        assert!(rendered.contains("Sale unavailable"));
        assert!(FleetFlow::start(&state).is_err());
        assert!(matches!(
            sell_train(&mut state, train_id),
            Err(crate::sim::fleet::FleetError::TrainTravelling { .. })
        ));
    }
}

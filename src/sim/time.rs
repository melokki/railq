//! Timestamp-driven operating-state advancement.
//!
//! Advancing accepts an explicit timestamp so callers at the application
//! boundary can use either the wall clock or a deterministic test time. It
//! never moves time backwards: equal and earlier timestamps only reconcile
//! Journeys that were already due.

use std::{error::Error, fmt};

use crate::{
    model::{
        CalculationError, GameState, Journey, JourneyId, JourneyReceipt, Money, RailStationId,
        TrainId, TrainStatus, UtcSeconds,
    },
    sim::demand::replenish_directional_demand,
};

/// Why a due Journey could not be settled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvanceTimeError {
    /// A stored monetary total cannot represent the arrival revenue.
    Calculation(CalculationError),
    /// A Journey refers to a Train no longer owned by the Player Company.
    TrainNotFound { train_id: TrainId },
    /// A Journey's Train is not travelling on that Journey.
    TrainNotTravelling {
        train_id: TrainId,
        journey_id: JourneyId,
    },
}

impl fmt::Display for AdvanceTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Calculation(error) => error.fmt(formatter),
            Self::TrainNotFound { train_id } => write!(
                formatter,
                "Journey cannot settle because Train {} is not in the Fleet",
                train_id.get()
            ),
            Self::TrainNotTravelling {
                train_id,
                journey_id,
            } => write!(
                formatter,
                "Journey {} cannot settle because Train {} is not travelling on it",
                journey_id.get(),
                train_id.get()
            ),
        }
    }
}

impl Error for AdvanceTimeError {}

impl From<CalculationError> for AdvanceTimeError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// One Journey settlement produced by a single reconciliation pass.
///
/// This is an in-memory application handoff, not save data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettledJourney {
    pub journey_id: JourneyId,
    pub train_id: TrainId,
    pub destination_station_id: RailStationId,
    pub credited_revenue: Money,
}

/// Advances demand and settles every Journey due at `now`.
///
/// The effective timestamp is the later of `now` and the last processed time.
/// Every due Journey is removed before a later call can observe it again, so
/// equal or backward timestamps cannot credit Operating Revenue twice.
pub fn advance_time(state: &mut GameState, now: UtcSeconds) -> Result<(), AdvanceTimeError> {
    advance_time_with_arrivals(state, now).map(|_| ())
}

/// Advances time and returns the Journeys settled during this call.
///
/// Callers publish the outcomes only after the candidate state is saved.
pub fn advance_time_with_arrivals(
    state: &mut GameState,
    now: UtcSeconds,
) -> Result<Vec<SettledJourney>, AdvanceTimeError> {
    let effective_now = now.max(state.last_processed_at);
    let due_journeys: Vec<Journey> = state
        .active_journeys
        .iter()
        .filter(|journey| journey.arrives_at <= effective_now)
        .cloned()
        .collect();

    let revenue_due = due_journeys
        .iter()
        .try_fold(Money::ZERO, |total, journey| {
            total.checked_add(journey.operating_revenue)
        })?;
    let funds_after_arrivals = state.player_company.funds.checked_add(revenue_due)?;
    let revenue_after_arrivals = state
        .financials
        .operating_revenue
        .checked_add(revenue_due)?;

    for journey in &due_journeys {
        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == journey.train_id)
            .ok_or(AdvanceTimeError::TrainNotFound {
                train_id: journey.train_id,
            })?;
        if train.status
            != (TrainStatus::Travelling {
                journey_id: journey.id,
            })
        {
            return Err(AdvanceTimeError::TrainNotTravelling {
                train_id: journey.train_id,
                journey_id: journey.id,
            });
        }
    }

    // Demand owns `last_processed_at`; doing it only after all fallible
    // settlement preparation keeps an invalid arrival from partially advancing
    // the operating state.
    replenish_directional_demand(state, effective_now);

    let settled_receipts = due_journeys
        .iter()
        .map(|journey| {
            let train = state
                .player_company
                .fleet
                .trains
                .iter()
                .find(|train| train.id == journey.train_id)
                .expect("due Journey Train was validated before state mutation");
            JourneyReceipt {
                journey_id: journey.id,
                revenue: journey.operating_revenue,
                infrastructure_access_fee: journey.infrastructure_access_fee,
                fuel_cost: journey.fuel_cost,
                train_id: Some(train.id),
                train_model_name: Some(train.model_name.clone()),
                origin_station_id: Some(journey.origin_station_id),
                destination_station_id: Some(journey.destination_station_id),
                passengers_carried: Some(journey.passengers_carried),
                passenger_capacity: Some(train.passenger_capacity.passengers()),
                completed_at: Some(journey.arrives_at),
            }
        })
        .collect::<Vec<_>>();

    for journey in &due_journeys {
        let train = state
            .player_company
            .fleet
            .trains
            .iter_mut()
            .find(|train| train.id == journey.train_id)
            .expect("due Journey Train was validated before state mutation");
        train.status = TrainStatus::Ready {
            at: journey.destination_station_id,
        };
    }
    state
        .active_journeys
        .retain(|journey| journey.arrives_at > effective_now);
    state.player_company.funds = funds_after_arrivals;
    state.financials.operating_revenue = revenue_after_arrivals;
    state
        .financials
        .recent_journey_receipts
        .extend(settled_receipts);

    Ok(due_journeys
        .into_iter()
        .map(|journey| SettledJourney {
            journey_id: journey.id,
            train_id: journey.train_id,
            destination_station_id: journey.destination_station_id,
            credited_revenue: journey.operating_revenue,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{Money, RailStationId, TrainStatus, UtcSeconds},
        sim::{
            economy::quote_journey, fleet::purchase_train, journeys::dispatch_journey,
            services::find_or_create_service, world::create_new_game,
        },
    };

    use super::advance_time;

    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);
    const DEPARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn dispatched_game() -> (
        crate::model::GameState,
        crate::model::TrainId,
        crate::model::JourneyId,
        crate::sim::economy::JourneyQuote,
    ) {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let quote = quote_journey(&state, train_id, service_id).unwrap();
        let journey_id = dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT).unwrap();
        (state, train_id, journey_id, quote)
    }

    #[test]
    fn before_eta_keeps_the_train_travelling() {
        let (mut state, _, journey_id, _) = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        let before_arrival = UtcSeconds::from_unix_seconds(arrives_at.unix_seconds() - 1);
        assert!(before_arrival < arrives_at);

        advance_time(&mut state, before_arrival).unwrap();

        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Travelling { journey_id }
        );
        assert_eq!(state.active_journeys.len(), 1);
        assert_eq!(state.financials.operating_revenue, Money::ZERO);
    }

    #[test]
    fn arrival_credits_revenue_once_and_releases_the_train() {
        let (mut state, _, journey_id, quote) = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        let funds_before_arrival = state.player_company.funds;

        advance_time(&mut state, arrives_at).unwrap();

        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Ready { at: DESTINATION }
        );
        assert_eq!(state.active_journeys, []);
        assert_eq!(
            state.player_company.funds,
            funds_before_arrival
                .checked_add(quote.operating_revenue)
                .unwrap()
        );
        assert_eq!(state.financials.operating_revenue, quote.operating_revenue);
        assert_eq!(state.financials.recent_journey_receipts.len(), 1);
        let receipt = &state.financials.recent_journey_receipts[0];
        assert_eq!(receipt.journey_id, journey_id);
        assert_eq!(receipt.train_id, Some(state.player_company.fleet.trains[0].id));
        assert_eq!(
            receipt.train_model_name.as_deref(),
            Some(state.player_company.fleet.trains[0].model_name.as_str())
        );
        assert_eq!(receipt.origin_station_id, Some(ORIGIN));
        assert_eq!(receipt.destination_station_id, Some(DESTINATION));
        assert_eq!(receipt.passengers_carried, Some(quote.boarded_passengers));
        assert_eq!(
            receipt.passenger_capacity,
            Some(state.player_company.fleet.trains[0].passenger_capacity.passengers())
        );
        assert_eq!(receipt.completed_at, Some(arrives_at));

        let after_first_arrival = state.clone();
        advance_time(&mut state, arrives_at).unwrap();
        assert_eq!(state, after_first_arrival);
    }

    #[test]
    fn earlier_timestamp_cannot_undo_or_repeat_settlement() {
        let (mut state, _, _, _) = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).unwrap();
        let after_arrival = state.clone();

        advance_time(
            &mut state,
            UtcSeconds::from_unix_seconds(arrives_at.unix_seconds() - 1),
        )
        .unwrap();

        assert_eq!(state, after_arrival);
    }

    #[test]
    fn settles_multiple_due_arrivals() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(1_000_000);
        let first_train = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let second_train = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let first_quote = quote_journey(&state, first_train, service_id).unwrap();
        let first_journey =
            dispatch_journey(&mut state, first_train, service_id, DEPARTED_AT).unwrap();
        let second_quote = quote_journey(&state, second_train, service_id).unwrap();
        let second_journey =
            dispatch_journey(&mut state, second_train, service_id, DEPARTED_AT).unwrap();
        let arrives_at = state.active_journeys[0].arrives_at;
        let funds_before_arrivals = state.player_company.funds;

        advance_time(&mut state, arrives_at).unwrap();

        assert_eq!(state.active_journeys, []);
        assert_eq!(
            state.player_company.funds,
            funds_before_arrivals
                .checked_add(first_quote.operating_revenue)
                .unwrap()
                .checked_add(second_quote.operating_revenue)
                .unwrap()
        );
        assert_eq!(state.financials.recent_journey_receipts.len(), 2);
        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Ready { at: DESTINATION }
        );
        assert_eq!(
            state.player_company.fleet.trains[1].status,
            TrainStatus::Ready { at: DESTINATION }
        );
        assert_eq!(
            state.financials.recent_journey_receipts[0].journey_id,
            first_journey
        );
        assert_eq!(
            state.financials.recent_journey_receipts[1].journey_id,
            second_journey
        );
    }
}

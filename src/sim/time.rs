//! Timestamp-driven operating-state advancement.
//!
//! Active Journeys progress from Service stop to Service stop using wall-clock
//! timestamps. Offline reconciliation may cross several intermediate stops in
//! one call; demand is advanced to each stop arrival before passengers alight
//! and new passengers board.

use std::{collections::HashMap, error::Error, fmt};

use crate::{
    catalog::model_for_train,
    model::{
        CalculationError, GameState, JourneyId, JourneyPassengerGroup, JourneyReceipt, Money,
        RailStationId, ServiceId, TrainId, TrainStatus, UtcSeconds,
    },
    sim::{
        authority::{
            advance_infrastructure_planning, advance_project_funding, advance_project_scheduling,
        },
        demand::replenish_directional_demand,
        economy::{EconomyError, duration_between_service_stops, quote_boarding_at_stop},
    },
};

/// Why a due Journey could not be advanced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvanceTimeError {
    Calculation(CalculationError),
    Economy(EconomyError),
    TrainNotFound {
        train_id: TrainId,
    },
    TrainModelNotFound {
        train_id: TrainId,
    },
    ServiceNotFound {
        service_id: ServiceId,
    },
    TrainNotTravelling {
        train_id: TrainId,
        journey_id: JourneyId,
    },
    InvalidServiceProgress {
        journey_id: JourneyId,
    },
    WaitingPassengersUnavailable {
        origin_station_id: RailStationId,
        destination_station_id: RailStationId,
    },
}

impl fmt::Display for AdvanceTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Calculation(error) => error.fmt(formatter),
            Self::Economy(error) => error.fmt(formatter),
            Self::TrainNotFound { train_id } => write!(
                formatter,
                "Journey cannot advance because Train {} is not in the Fleet",
                train_id.get()
            ),
            Self::TrainModelNotFound { train_id } => write!(
                formatter,
                "Journey cannot advance because Train {} references a missing catalogue model",
                train_id.get()
            ),
            Self::ServiceNotFound { service_id } => write!(
                formatter,
                "Journey cannot advance because Passenger Service {} no longer exists",
                service_id.get()
            ),
            Self::TrainNotTravelling {
                train_id,
                journey_id,
            } => write!(
                formatter,
                "Journey {} cannot advance because Train {} is not travelling on it",
                journey_id.get(),
                train_id.get()
            ),
            Self::InvalidServiceProgress { journey_id } => write!(
                formatter,
                "Journey {} has an invalid current Service stop",
                journey_id.get()
            ),
            Self::WaitingPassengersUnavailable {
                origin_station_id,
                destination_station_id,
            } => write!(
                formatter,
                "Waiting Passengers from Rail Station {} to Rail Station {} changed before boarding",
                origin_station_id.get(),
                destination_station_id.get()
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

impl From<EconomyError> for AdvanceTimeError {
    fn from(error: EconomyError) -> Self {
        Self::Economy(error)
    }
}

/// One completed Service run produced by a reconciliation pass.
///
/// Intermediate stop calls do not remove the active Journey and therefore do
/// not produce a `SettledJourney`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettledJourney {
    pub journey_id: JourneyId,
    pub train_id: TrainId,
    pub destination_station_id: RailStationId,
    /// Revenue credited for this Journey during the current reconciliation.
    ///
    /// When an offline catch-up crosses several stops, this includes every
    /// stop credit processed in that call. Revenue credited by earlier calls
    /// is not repeated.
    pub credited_revenue: Money,
}

pub fn advance_time(state: &mut GameState, now: UtcSeconds) -> Result<(), AdvanceTimeError> {
    advance_time_with_arrivals(state, now).map(|_| ())
}

/// Advances Passenger Demand and every active Journey up to `now`.
///
/// Stop events are processed chronologically. This means reopening RailQ after
/// a long offline gap produces the same boarding/alighting result as processing
/// each intermediate stop while the application remained open.
pub fn advance_time_with_arrivals(
    state: &mut GameState,
    now: UtcSeconds,
) -> Result<Vec<SettledJourney>, AdvanceTimeError> {
    let effective_now = now.max(state.last_processed_at);
    let mut settled = Vec::new();
    let mut credited_this_pass = HashMap::<JourneyId, Money>::new();

    loop {
        let Some(journey_index) = next_due_journey_index(state, effective_now) else {
            break;
        };
        let journey_id = state.active_journeys[journey_index].id;
        let arrival_time = state.active_journeys[journey_index].arrives_at;

        // Demand must exist at the exact stop-arrival timestamp before new
        // passengers are selected for the next leg.
        replenish_directional_demand(state, arrival_time);

        let (completed, credited_now) = process_stop_arrival(state, journey_index)?;
        let credited_total = credited_this_pass
            .get(&journey_id)
            .copied()
            .unwrap_or(Money::ZERO)
            .checked_add(credited_now)?;
        credited_this_pass.insert(journey_id, credited_total);

        if let Some(mut completed) = completed {
            // Report all revenue credited during this reconciliation pass,
            // including any intermediate stops crossed while RailQ was closed.
            completed.credited_revenue = credited_total;
            credited_this_pass.remove(&journey_id);
            settled.push(completed);
        }
    }

    replenish_directional_demand(state, effective_now);
    advance_infrastructure_planning(&mut state.region, state.world_seed, effective_now)?;
    advance_project_funding(&mut state.region, effective_now)?;
    advance_project_scheduling(&mut state.region, effective_now)?;
    Ok(settled)
}

fn next_due_journey_index(state: &GameState, effective_now: UtcSeconds) -> Option<usize> {
    state
        .active_journeys
        .iter()
        .enumerate()
        .filter(|(_, journey)| journey.arrives_at <= effective_now)
        .min_by_key(|(_, journey)| (journey.arrives_at, journey.id))
        .map(|(index, _)| index)
}

fn process_stop_arrival(
    state: &mut GameState,
    journey_index: usize,
) -> Result<(Option<SettledJourney>, Money), AdvanceTimeError> {
    let journey_snapshot = state.active_journeys[journey_index].clone();
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey_snapshot.service_id)
        .cloned()
        .ok_or(AdvanceTimeError::ServiceNotFound {
            service_id: journey_snapshot.service_id,
        })?;
    let train_index = state
        .player_company
        .fleet
        .trains
        .iter()
        .position(|train| train.id == journey_snapshot.train_id)
        .ok_or(AdvanceTimeError::TrainNotFound {
            train_id: journey_snapshot.train_id,
        })?;
    let train_model = model_for_train(&state.player_company.fleet.trains[train_index]).ok_or(
        AdvanceTimeError::TrainModelNotFound {
            train_id: journey_snapshot.train_id,
        },
    )?;
    if state.player_company.fleet.trains[train_index].status
        != (TrainStatus::Travelling {
            journey_id: journey_snapshot.id,
        })
    {
        return Err(AdvanceTimeError::TrainNotTravelling {
            train_id: journey_snapshot.train_id,
            journey_id: journey_snapshot.id,
        });
    }

    let direction = service_direction(&service, &journey_snapshot).ok_or(
        AdvanceTimeError::InvalidServiceProgress {
            journey_id: journey_snapshot.id,
        },
    )?;
    let arrival_stop_index = next_stop_index(
        journey_snapshot.current_stop_index,
        direction,
        service.stop_station_ids.len(),
    )
    .ok_or(AdvanceTimeError::InvalidServiceProgress {
        journey_id: journey_snapshot.id,
    })?;
    let arrival_station_id = service.stop_station_ids[arrival_stop_index];
    let final_arrival = arrival_station_id == journey_snapshot.destination_station_id;

    let mut remaining_groups = Vec::new();
    let mut credited_now = Money::ZERO;
    for group in &journey_snapshot.passenger_groups {
        if group.destination_station_id == arrival_station_id {
            credited_now =
                credited_now.checked_add(group.fare.checked_mul(u64::from(group.passengers))?)?;
        } else {
            remaining_groups.push(group.clone());
        }
    }

    let onboard_after_alighting = remaining_groups.iter().try_fold(0_u32, |total, group| {
        total
            .checked_add(group.passengers)
            .ok_or(CalculationError::Overflow {
                operation: "onboard passenger count",
            })
    })?;
    let capacity = train_model.passenger_capacity().passengers();

    if final_arrival {
        if !remaining_groups.is_empty() {
            return Err(AdvanceTimeError::InvalidServiceProgress {
                journey_id: journey_snapshot.id,
            });
        }

        let funds = state.player_company.funds.checked_add(credited_now)?;
        let operating_revenue = state
            .financials
            .operating_revenue
            .checked_add(credited_now)?;
        let credited_total = journey_snapshot
            .credited_revenue
            .checked_add(credited_now)?;
        if credited_total != journey_snapshot.operating_revenue {
            return Err(AdvanceTimeError::InvalidServiceProgress {
                journey_id: journey_snapshot.id,
            });
        }

        state.player_company.funds = funds;
        state.financials.operating_revenue = operating_revenue;
        state.player_company.fleet.trains[train_index].status = TrainStatus::Ready {
            at: arrival_station_id,
        };

        let receipt = JourneyReceipt {
            journey_id: journey_snapshot.id,
            revenue: journey_snapshot.operating_revenue,
            infrastructure_access_fee: journey_snapshot.infrastructure_access_fee,
            fuel_cost: journey_snapshot.fuel_cost,
            train_id: Some(journey_snapshot.train_id),
            train_model_name: Some(train_model.name().to_owned()),
            origin_station_id: Some(journey_snapshot.origin_station_id),
            destination_station_id: Some(journey_snapshot.destination_station_id),
            passengers_carried: Some(journey_snapshot.passengers_carried),
            passenger_capacity: Some(capacity),
            completed_at: Some(journey_snapshot.arrives_at),
        };
        state.financials.recent_journey_receipts.push(receipt);
        state.active_journeys.remove(journey_index);

        return Ok((
            Some(SettledJourney {
                journey_id: journey_snapshot.id,
                train_id: journey_snapshot.train_id,
                destination_station_id: arrival_station_id,
                credited_revenue: credited_now,
            }),
            credited_now,
        ));
    }

    let free_capacity = capacity.saturating_sub(onboard_after_alighting);
    let boarding_quotes = if direction > 0 {
        quote_boarding_at_stop(state, &service, arrival_stop_index, free_capacity)?
    } else {
        // Reverse active Journeys can only exist as compatibility snapshots
        // from the old bidirectional Service model. They are allowed to finish
        // safely, but new directional Services never dispatch in reverse.
        Vec::new()
    };

    let mut demand_deductions = Vec::new();
    for boarding in &boarding_quotes {
        let demand_index = state
            .origin_destination_demand
            .iter()
            .position(|demand| {
                demand.origin_station_id == boarding.origin_station_id
                    && demand.destination_station_id == boarding.destination_station_id
            })
            .ok_or(AdvanceTimeError::WaitingPassengersUnavailable {
                origin_station_id: boarding.origin_station_id,
                destination_station_id: boarding.destination_station_id,
            })?;
        let remaining = state.origin_destination_demand[demand_index]
            .waiting_passengers
            .checked_sub(boarding.passengers)
            .ok_or(AdvanceTimeError::WaitingPassengersUnavailable {
                origin_station_id: boarding.origin_station_id,
                destination_station_id: boarding.destination_station_id,
            })?;
        demand_deductions.push((demand_index, remaining));
    }

    let newly_boarded = boarding_quotes.iter().try_fold(0_u32, |total, boarding| {
        total
            .checked_add(boarding.passengers)
            .ok_or(CalculationError::Overflow {
                operation: "Journey passenger boardings",
            })
    })?;
    let newly_booked_revenue = boarding_quotes
        .iter()
        .try_fold(Money::ZERO, |total, boarding| {
            total.checked_add(boarding.revenue)
        })?;
    let passengers_carried = journey_snapshot
        .passengers_carried
        .checked_add(newly_boarded)
        .ok_or(CalculationError::Overflow {
            operation: "Journey passengers carried",
        })?;
    let booked_revenue = journey_snapshot
        .operating_revenue
        .checked_add(newly_booked_revenue)?;
    let credited_revenue = journey_snapshot
        .credited_revenue
        .checked_add(credited_now)?;

    let next_leg_stop_index = next_stop_index(
        arrival_stop_index,
        direction,
        service.stop_station_ids.len(),
    )
    .ok_or(AdvanceTimeError::InvalidServiceProgress {
        journey_id: journey_snapshot.id,
    })?;
    let next_leg_duration = duration_between_service_stops(
        state,
        &service,
        arrival_stop_index,
        next_leg_stop_index,
        train_model.speed(),
    )?;
    let next_arrival = journey_snapshot.arrives_at.checked_add(next_leg_duration)?;

    let funds = state.player_company.funds.checked_add(credited_now)?;
    let operating_revenue = state
        .financials
        .operating_revenue
        .checked_add(credited_now)?;

    for (index, remaining) in demand_deductions {
        state.origin_destination_demand[index].waiting_passengers = remaining;
    }
    state.player_company.funds = funds;
    state.financials.operating_revenue = operating_revenue;

    let journey = &mut state.active_journeys[journey_index];
    journey.current_stop_index = arrival_stop_index;
    journey.passengers_carried = passengers_carried;
    journey.operating_revenue = booked_revenue;
    journey.credited_revenue = credited_revenue;
    journey.passenger_groups = remaining_groups;
    journey
        .passenger_groups
        .extend(
            boarding_quotes
                .into_iter()
                .map(|boarding| JourneyPassengerGroup {
                    origin_station_id: boarding.origin_station_id,
                    destination_station_id: boarding.destination_station_id,
                    passengers: boarding.passengers,
                    fare: boarding.fare,
                }),
        );
    journey.departed_at = journey_snapshot.arrives_at;
    journey.arrives_at = next_arrival;

    Ok((None, credited_now))
}

fn service_direction(
    service: &crate::model::PassengerService,
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

fn next_stop_index(current: usize, direction: i32, stop_count: usize) -> Option<usize> {
    match direction {
        1 => current.checked_add(1).filter(|index| *index < stop_count),
        -1 => current.checked_sub(1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        catalog::model_for_train,
        model::{Money, RailStationId, TrainStatus, UtcSeconds},
        sim::{
            economy::quote_journey,
            fleet::purchase_train,
            journeys::dispatch_journey,
            services::{create_service, find_or_create_service},
            world::create_new_game,
        },
    };

    use super::{advance_time, advance_time_with_arrivals};

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
        assert_eq!(
            receipt.train_id,
            Some(state.player_company.fleet.trains[0].id)
        );
        let train_model = model_for_train(&state.player_company.fleet.trains[0])
            .expect("fixture Train model remains in the embedded catalogue");
        assert_eq!(
            receipt.train_model_name.as_deref(),
            Some(train_model.name())
        );
        assert_eq!(receipt.origin_station_id, Some(ORIGIN));
        assert_eq!(receipt.destination_station_id, Some(DESTINATION));
        assert_eq!(receipt.passengers_carried, Some(quote.boarded_passengers));
        assert_eq!(
            receipt.passenger_capacity,
            Some(train_model.passenger_capacity().passengers())
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
    #[test]
    fn multi_stop_service_alights_boards_and_continues_under_one_journey() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(1_000_000);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id = create_service(
            &mut state,
            vec![
                RailStationId::new(1),
                RailStationId::new(2),
                RailStationId::new(3),
            ],
        )
        .unwrap();

        for pool in &mut state.origin_destination_demand {
            pool.waiting_passengers = 0;
            pool.fractional_passenger_seconds = 0;
        }
        for (origin, destination, passengers) in [
            (RailStationId::new(1), RailStationId::new(2), 10),
            (RailStationId::new(1), RailStationId::new(3), 20),
            (RailStationId::new(2), RailStationId::new(3), 15),
        ] {
            state
                .origin_destination_demand
                .iter_mut()
                .find(|pool| {
                    pool.origin_station_id == origin && pool.destination_station_id == destination
                })
                .unwrap()
                .waiting_passengers = passengers;
        }

        let journey_id = dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT).unwrap();
        assert_eq!(state.active_journeys[0].onboard_passengers(), 30);
        assert_eq!(state.active_journeys[0].passengers_carried, 30);

        let first_arrival = state.active_journeys[0].arrives_at;
        state.last_processed_at = first_arrival;
        advance_time(&mut state, first_arrival).unwrap();

        assert_eq!(state.active_journeys.len(), 1);
        let journey = &state.active_journeys[0];
        assert_eq!(journey.id, journey_id);
        assert_eq!(journey.current_stop_index, 1);
        assert_eq!(journey.passengers_carried, 45);
        assert_eq!(journey.onboard_passengers(), 35);
        assert!(journey.credited_revenue > Money::ZERO);
        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Travelling { journey_id }
        );

        let terminus_arrival = journey.arrives_at;
        state.last_processed_at = terminus_arrival;
        advance_time(&mut state, terminus_arrival).unwrap();

        assert!(state.active_journeys.is_empty());
        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Ready {
                at: RailStationId::new(3)
            }
        );
        let receipt = state.financials.recent_journey_receipts.last().unwrap();
        assert_eq!(receipt.journey_id, journey_id);
        assert_eq!(receipt.passengers_carried, Some(45));
        assert!(receipt.revenue > Money::ZERO);
    }

    #[test]
    fn offline_reconciliation_reports_revenue_credited_across_all_service_stops() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(1_000_000);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id = create_service(
            &mut state,
            vec![
                RailStationId::new(1),
                RailStationId::new(2),
                RailStationId::new(3),
            ],
        )
        .unwrap();

        let journey_id = dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT).unwrap();
        let far_future = UtcSeconds::from_unix_seconds(DEPARTED_AT.unix_seconds() + 86_400);

        let settled = advance_time_with_arrivals(&mut state, far_future).unwrap();

        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].journey_id, journey_id);
        let receipt = state
            .financials
            .recent_journey_receipts
            .iter()
            .find(|receipt| receipt.journey_id == journey_id)
            .unwrap();
        assert_eq!(settled[0].credited_revenue, receipt.revenue);
    }
}

//! Manual Journey dispatch.
//!
//! Dispatch recalculates a quote from the current state, then commits every
//! departure effect together: Company Funds and Waiting Passengers decrease,
//! the Train begins travelling, and one active Journey records the accepted
//! commercial terms. Operating Revenue remains due until arrival.

use std::{error::Error, fmt};

use crate::{
    model::{
        CalculationError, GameState, Journey, JourneyId, Money, RailStationId, ServiceId, TrainId,
        TrainStatus, UtcSeconds,
    },
    sim::economy::{EconomyError, quote_journey},
};

/// Why a manual Journey cannot depart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchError {
    /// The current state cannot produce a valid Journey quote.
    Quote(EconomyError),
    /// Company Funds cannot cover the known departure operating costs.
    InsufficientCompanyFunds { available: Money, required: Money },
    /// The current Waiting Passengers pool cannot supply the quoted boarding count.
    WaitingPassengersUnavailable {
        origin_station_id: RailStationId,
        destination_station_id: RailStationId,
    },
    /// A new Journey ID cannot be represented.
    JourneyIdExhausted,
    /// A checked calculation could not be represented.
    Calculation(CalculationError),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quote(error) => error.fmt(formatter),
            Self::InsufficientCompanyFunds {
                available,
                required,
            } => write!(
                formatter,
                "Company Funds of {} cents cannot cover Journey departure costs of {} cents",
                available.cents(),
                required.cents()
            ),
            Self::WaitingPassengersUnavailable {
                origin_station_id,
                destination_station_id,
            } => write!(
                formatter,
                "Waiting Passengers from Rail Station {} to Rail Station {} changed before departure",
                origin_station_id.get(),
                destination_station_id.get()
            ),
            Self::JourneyIdExhausted => write!(formatter, "Journey IDs are exhausted"),
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for DispatchError {}

impl From<CalculationError> for DispatchError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Explicitly authorises one Journey for a READY Train.
///
/// The proposed departure is deliberately not accepted as a stale quote.
/// Instead, the current state is quoted again before any effect is committed.
/// Consequently a Train that moved, began another Journey, or lost access to
/// sufficient Company Funds is rejected without changing state.
pub fn dispatch_journey(
    state: &mut GameState,
    train_id: TrainId,
    service_id: ServiceId,
    departed_at: UtcSeconds,
) -> Result<JourneyId, DispatchError> {
    let quote = quote_journey(state, train_id, service_id).map_err(DispatchError::Quote)?;
    if state.player_company.funds < quote.operating_cost {
        return Err(DispatchError::InsufficientCompanyFunds {
            available: state.player_company.funds,
            required: quote.operating_cost,
        });
    }

    let journey_id = next_journey_id(&state.active_journeys)?;
    let arrives_at = departed_at.checked_add(quote.duration)?;
    let funds_after_departure = state
        .player_company
        .funds
        .checked_sub(quote.operating_cost)?;
    let access_fees_after_departure = state
        .financials
        .infrastructure_access_fees
        .checked_add(quote.infrastructure_access_fee)?;
    let fuel_costs_after_departure = state.financials.fuel_costs.checked_add(quote.fuel_cost)?;
    let demand_index = state
        .origin_destination_demand
        .iter()
        .position(|demand| {
            demand.origin_station_id == quote.origin_station_id
                && demand.destination_station_id == quote.destination_station_id
        })
        .ok_or(DispatchError::WaitingPassengersUnavailable {
            origin_station_id: quote.origin_station_id,
            destination_station_id: quote.destination_station_id,
        })?;
    let remaining_waiting_passengers = state.origin_destination_demand[demand_index]
        .waiting_passengers
        .checked_sub(quote.boarded_passengers)
        .ok_or(DispatchError::WaitingPassengersUnavailable {
            origin_station_id: quote.origin_station_id,
            destination_station_id: quote.destination_station_id,
        })?;
    let train_index = state
        .player_company
        .fleet
        .trains
        .iter()
        .position(|train| train.id == train_id)
        .ok_or(DispatchError::Quote(EconomyError::TrainNotFound {
            train_id,
        }))?;

    state.player_company.funds = funds_after_departure;
    state.financials.infrastructure_access_fees = access_fees_after_departure;
    state.financials.fuel_costs = fuel_costs_after_departure;
    state.origin_destination_demand[demand_index].waiting_passengers = remaining_waiting_passengers;
    state.player_company.fleet.trains[train_index].status = TrainStatus::Travelling { journey_id };
    state.active_journeys.push(Journey {
        id: journey_id,
        service_id: quote.service_id,
        train_id: quote.train_id,
        origin_station_id: quote.origin_station_id,
        destination_station_id: quote.destination_station_id,
        passengers_carried: quote.boarded_passengers,
        fare: quote.fare,
        operating_revenue: quote.operating_revenue,
        infrastructure_access_fee: quote.infrastructure_access_fee,
        fuel_cost: quote.fuel_cost,
        departed_at,
        arrives_at,
    });
    Ok(journey_id)
}

fn next_journey_id(active_journeys: &[Journey]) -> Result<JourneyId, DispatchError> {
    active_journeys
        .iter()
        .map(|journey| journey.id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .map(JourneyId::new)
        .ok_or(DispatchError::JourneyIdExhausted)
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{Money, RailStationId, TrainStatus, UtcSeconds},
        sim::{
            economy::quote_journey, fleet::purchase_train, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{DispatchError, dispatch_journey};

    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);
    const DEPARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn prepared_game() -> (
        crate::model::GameState,
        crate::model::TrainId,
        crate::model::ServiceId,
    ) {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        (state, train_id, service_id)
    }

    #[test]
    fn dispatch_revalidates_current_location_before_changing_state() {
        let (mut state, train_id, service_id) = prepared_game();
        let _stale_quote = quote_journey(&state, train_id, service_id).unwrap();
        state.player_company.fleet.trains[0].status = TrainStatus::Ready {
            at: RailStationId::new(3),
        };
        let before = state.clone();

        let result = dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT);

        assert!(matches!(result, Err(DispatchError::Quote(_))));
        assert_eq!(state, before);
    }

    #[test]
    fn departure_applies_current_quote_once_and_creates_an_active_journey() {
        let (mut state, train_id, service_id) = prepared_game();
        let quote = quote_journey(&state, train_id, service_id).unwrap();
        let waiting_before = state.origin_destination_demand[0].waiting_passengers;

        let journey_id = dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT).unwrap();

        assert_eq!(state.player_company.funds, quote.cash_after_cost);
        assert_eq!(
            state.origin_destination_demand[0].waiting_passengers,
            waiting_before - quote.boarded_passengers
        );
        assert_eq!(
            state.financials.infrastructure_access_fees,
            quote.infrastructure_access_fee
        );
        assert_eq!(state.financials.fuel_costs, quote.fuel_cost);
        assert_eq!(
            state.player_company.fleet.trains[0].status,
            TrainStatus::Travelling { journey_id }
        );
        assert_eq!(state.active_journeys.len(), 1);
        let journey = &state.active_journeys[0];
        assert_eq!(journey.id, journey_id);
        assert_eq!(journey.passengers_carried, quote.boarded_passengers);
        assert_eq!(journey.fare, quote.fare);
        assert_eq!(journey.operating_revenue, quote.operating_revenue);
        assert_eq!(
            journey.infrastructure_access_fee,
            quote.infrastructure_access_fee
        );
        assert_eq!(journey.fuel_cost, quote.fuel_cost);
        assert_eq!(journey.departed_at, DEPARTED_AT);
        assert_eq!(
            journey.arrives_at,
            DEPARTED_AT.checked_add(quote.duration).unwrap()
        );

        let before_second_dispatch = state.clone();
        assert!(matches!(
            dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT),
            Err(DispatchError::Quote(_))
        ));
        assert_eq!(state, before_second_dispatch);
    }

    #[test]
    fn insufficient_funds_leave_state_unchanged() {
        let (mut state, train_id, service_id) = prepared_game();
        let quote = quote_journey(&state, train_id, service_id).unwrap();
        state.player_company.funds = Money::from_cents(quote.operating_cost.cents() - 1);
        let before = state.clone();

        assert_eq!(
            dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT),
            Err(DispatchError::InsufficientCompanyFunds {
                available: before.player_company.funds,
                required: quote.operating_cost,
            })
        );
        assert_eq!(state, before);
    }
}

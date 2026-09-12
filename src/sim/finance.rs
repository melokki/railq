//! Financial failure and recovery evaluation.
//!
//! The evaluator is deliberately read-only. It examines finite candidate
//! states with the same purchase, resale, Service, and Journey quote rules as
//! live play, so a reported option is affordable at the Train's actual
//! location without changing the Player Company's game state.

use std::{error::Error, fmt};

use crate::{
    model::{CalculationError, GameState, Money, RailStationId, TrainId, TrainStatus},
    sim::{
        economy::{EconomyError, quote_journey},
        fleet::{FleetError, purchase_train, sell_train},
        services::{ServiceError, find_or_create_service},
    },
};

/// The Player Company's present operating or financial-failure state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinancialStatus {
    /// Current Company Funds can pay the departure costs of at least one Journey.
    Operating,
    /// The company cannot currently dispatch, but a finite recovery option exists.
    Insolvent,
    /// A Journey is still active, so Bankruptcy must wait for its settlement.
    BankruptcyDeferred,
    /// No current Journey or finite recovery option can keep the company operating.
    Bankruptcy,
}

/// A Journey that is affordable in a recovery candidate state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryJourney {
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub operating_cost: Money,
}

/// A finite way for an Insolvent Player Company to operate again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryOption {
    /// An owned READY Train can depart using current Company Funds alone.
    CashOnly { journey: RecoveryJourney },
    /// Sell every other READY Train, retain one Train, then dispatch it.
    SellOthersAndRetain {
        retained_train_id: TrainId,
        sold_train_ids: Vec<TrainId>,
        resale_proceeds: Money,
        journey: RecoveryJourney,
    },
    /// Sell the Fleet, buy a catalogue Train at a Rail Station, then dispatch it.
    SellAllAndRebuy {
        sold_train_ids: Vec<TrainId>,
        resale_proceeds: Money,
        catalogue_index: usize,
        delivery_station_id: RailStationId,
        journey: RecoveryJourney,
    },
}

/// The complete read-only financial evaluation for the current game state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinancialEvaluation {
    pub status: FinancialStatus,
    /// Affordable Journeys available without selling from the Fleet.
    pub cash_only_options: Vec<RecoveryOption>,
    /// Affordable options that require selling and retaining or replacing Trains.
    pub recovery_options: Vec<RecoveryOption>,
}

/// Why a valid recovery candidate could not be evaluated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinanceError {
    Fleet(FleetError),
    Service(ServiceError),
    Economy(EconomyError),
    Calculation(CalculationError),
}

impl fmt::Display for FinanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fleet(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
            Self::Economy(error) => error.fmt(formatter),
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for FinanceError {}

impl From<FleetError> for FinanceError {
    fn from(error: FleetError) -> Self {
        Self::Fleet(error)
    }
}

impl From<ServiceError> for FinanceError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

impl From<EconomyError> for FinanceError {
    fn from(error: EconomyError) -> Self {
        Self::Economy(error)
    }
}

impl From<CalculationError> for FinanceError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Evaluates whether the Player Company can operate, recover, or is Bankrupt.
///
/// The evaluator first looks for a Journey that can be funded immediately.
/// An unrelated active Journey must not mask an otherwise healthy company.
/// Only when no current Journey can be funded does unsettled Operating Revenue
/// defer Bankruptcy. Once every active Journey has settled, the evaluator can
/// apply the finite recovery rules: sell all other Trains while retaining one,
/// or sell every Train and buy and dispatch catalogue stock. Passenger
/// Services do not constrain this check because creating one is free.
pub fn evaluate_financial_recovery(state: &GameState) -> Result<FinancialEvaluation, FinanceError> {
    let cash_only_options = cash_only_options(state)?;
    if !cash_only_options.is_empty() {
        return Ok(FinancialEvaluation {
            status: FinancialStatus::Operating,
            cash_only_options,
            recovery_options: Vec::new(),
        });
    }

    if !state.active_journeys.is_empty() {
        return Ok(FinancialEvaluation {
            status: FinancialStatus::BankruptcyDeferred,
            cash_only_options: Vec::new(),
            recovery_options: Vec::new(),
        });
    }

    let mut recovery_options = retained_fleet_options(state)?;
    recovery_options.extend(sell_all_and_rebuy_options(state)?);
    let status = if recovery_options.is_empty() {
        FinancialStatus::Bankruptcy
    } else {
        FinancialStatus::Insolvent
    };

    Ok(FinancialEvaluation {
        status,
        cash_only_options,
        recovery_options,
    })
}

fn cash_only_options(state: &GameState) -> Result<Vec<RecoveryOption>, FinanceError> {
    let mut options = Vec::new();
    for train in &state.player_company.fleet.trains {
        if matches!(train.status, TrainStatus::Ready { .. }) {
            let candidate = state.clone();
            options.extend(
                affordable_journeys(candidate, train.id)?
                    .into_iter()
                    .map(|journey| RecoveryOption::CashOnly { journey }),
            );
        }
    }
    Ok(options)
}

fn retained_fleet_options(state: &GameState) -> Result<Vec<RecoveryOption>, FinanceError> {
    if !all_trains_ready(state) {
        return Ok(Vec::new());
    }

    let mut options = Vec::new();
    for retained_train in &state.player_company.fleet.trains {
        let mut candidate = state.clone();
        let (sold_train_ids, resale_proceeds) = sell_all_except(&mut candidate, retained_train.id)?;
        if sold_train_ids.is_empty() {
            continue;
        }
        options.extend(
            affordable_journeys(candidate, retained_train.id)?
                .into_iter()
                .map(|journey| RecoveryOption::SellOthersAndRetain {
                    retained_train_id: retained_train.id,
                    sold_train_ids: sold_train_ids.clone(),
                    resale_proceeds,
                    journey,
                }),
        );
    }
    Ok(options)
}

fn sell_all_and_rebuy_options(state: &GameState) -> Result<Vec<RecoveryOption>, FinanceError> {
    if !all_trains_ready(state) {
        return Ok(Vec::new());
    }

    let mut sold_candidate = state.clone();
    let (sold_train_ids, resale_proceeds) = sell_all_except(&mut sold_candidate, None)?;
    let delivery_station_ids: Vec<_> = sold_candidate
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect();
    let catalogue_len = sold_candidate.rules.balance.diesel_catalogue().len();
    let mut options = Vec::new();

    for catalogue_index in 0..catalogue_len {
        for delivery_station_id in &delivery_station_ids {
            let mut candidate = sold_candidate.clone();
            let purchase_price =
                candidate.rules.balance.diesel_catalogue()[catalogue_index].purchase_price();
            if purchase_price.cents() <= 0 || candidate.player_company.funds < purchase_price {
                continue;
            }
            let train_id = purchase_train(&mut candidate, catalogue_index, *delivery_station_id)?;
            options.extend(
                affordable_journeys(candidate, train_id)?
                    .into_iter()
                    .map(|journey| RecoveryOption::SellAllAndRebuy {
                        sold_train_ids: sold_train_ids.clone(),
                        resale_proceeds,
                        catalogue_index,
                        delivery_station_id: *delivery_station_id,
                        journey,
                    }),
            );
        }
    }
    Ok(options)
}

fn all_trains_ready(state: &GameState) -> bool {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .all(|train| matches!(train.status, TrainStatus::Ready { .. }))
}

fn sell_all_except(
    state: &mut GameState,
    retained_train_id: impl Into<Option<TrainId>>,
) -> Result<(Vec<TrainId>, Money), FinanceError> {
    let retained_train_id = retained_train_id.into();
    let train_ids: Vec<_> = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| train.id)
        .filter(|train_id| Some(*train_id) != retained_train_id)
        .collect();
    let mut resale_proceeds = Money::ZERO;
    for train_id in &train_ids {
        resale_proceeds = resale_proceeds.checked_add(sell_train(state, *train_id)?)?;
    }
    Ok((train_ids, resale_proceeds))
}

fn affordable_journeys(
    mut candidate: GameState,
    train_id: TrainId,
) -> Result<Vec<RecoveryJourney>, FinanceError> {
    let origin_station_id = match candidate
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map(|train| &train.status)
    {
        Some(TrainStatus::Ready { at }) => *at,
        Some(TrainStatus::Travelling { .. }) | None => return Ok(Vec::new()),
    };
    let destination_station_ids: Vec<_> = candidate
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .filter(|station_id| *station_id != origin_station_id)
        .collect();
    let mut journeys = Vec::new();

    for destination_station_id in destination_station_ids {
        let service_id =
            match find_or_create_service(&mut candidate, origin_station_id, destination_station_id)
            {
                Ok(service_id) => service_id,
                Err(ServiceError::NoPath { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
        let quote = quote_journey(&candidate, train_id, service_id)?;
        if candidate.player_company.funds >= quote.operating_cost {
            journeys.push(RecoveryJourney {
                train_id,
                origin_station_id,
                destination_station_id,
                operating_cost: quote.operating_cost,
            });
        }
    }
    Ok(journeys)
}

#[cfg(test)]
mod tests {
    use crate::{
        balance::{BalanceConfig, DieselTrainCatalogueRecord},
        model::{MoneyPerKilometre, PassengerCapacity, SpeedMetresPerSecond, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::*;

    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);

    fn configured_game(funds: Money, catalogue: Vec<DieselTrainCatalogueRecord>) -> GameState {
        let mut state = create_new_game(42, "Recovery Passenger", UtcSeconds::from_unix_seconds(0));
        let rate = MoneyPerKilometre::new(10).unwrap();
        state.rules.balance = BalanceConfig::new(rate, rate, funds, catalogue);
        state.player_company.funds = funds;
        state
    }

    fn diesel(name: &str, price: Money) -> DieselTrainCatalogueRecord {
        DieselTrainCatalogueRecord::new(
            name,
            price,
            PassengerCapacity::new(10).unwrap(),
            SpeedMetresPerSecond::new(100).unwrap(),
            MoneyPerKilometre::new(5).unwrap(),
        )
    }

    #[test]
    fn active_journey_defers_bankruptcy() {
        let mut state = configured_game(
            Money::from_cents(1_150),
            vec![diesel("Local", Money::from_cents(1_000))],
        );
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(
            &mut state,
            train_id,
            service_id,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();
        assert_eq!(state.player_company.funds, Money::ZERO);

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::BankruptcyDeferred);
        assert!(evaluation.cash_only_options.is_empty());
        assert!(evaluation.recovery_options.is_empty());
    }

    #[test]
    fn active_journey_does_not_mask_an_affordable_ready_train() {
        let mut state = configured_game(
            Money::from_cents(2_300),
            vec![diesel("Local", Money::from_cents(1_000))],
        );
        let travelling_train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let ready_train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(
            &mut state,
            travelling_train_id,
            service_id,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::Operating);
        assert!(evaluation.cash_only_options.iter().any(|option| {
            matches!(option, RecoveryOption::CashOnly { journey }
                if journey.train_id == ready_train_id
                    && journey.origin_station_id == ORIGIN
                    && journey.destination_station_id == DESTINATION)
        }));
    }

    #[test]
    fn current_company_funds_can_restart_without_an_existing_service() {
        let mut state = configured_game(
            Money::from_cents(1_150),
            vec![diesel("Local", Money::from_cents(1_000))],
        );
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let before = state.clone();

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::Operating);
        assert_eq!(state, before);
        assert!(evaluation.cash_only_options.iter().any(|option| {
            matches!(option, RecoveryOption::CashOnly { journey }
                if journey.train_id == train_id
                    && journey.origin_station_id == ORIGIN
                    && journey.destination_station_id == DESTINATION
                    && journey.operating_cost == Money::from_cents(150))
        }));
    }

    #[test]
    fn selling_other_trains_can_recover_a_retained_train() {
        let mut state = configured_game(
            Money::from_cents(2_000),
            vec![diesel("Local", Money::from_cents(1_000))],
        );
        let retained_train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let sold_train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        state.player_company.funds = Money::ZERO;

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::Insolvent);
        assert!(evaluation.recovery_options.iter().any(|option| {
            matches!(option, RecoveryOption::SellOthersAndRetain {
                retained_train_id: option_retained_train_id,
                sold_train_ids,
                resale_proceeds,
                journey,
            } if *option_retained_train_id == retained_train_id
                && sold_train_ids == &vec![sold_train_id]
                && *resale_proceeds == Money::from_cents(700)
                && journey.operating_cost == Money::from_cents(150))
        }));
    }

    #[test]
    fn selling_all_trains_can_buy_and_dispatch_catalogue_stock() {
        let catalogue = vec![
            diesel("Local", Money::from_cents(500)),
            diesel("Express", Money::from_cents(2_000)),
        ];
        let mut state = configured_game(Money::from_cents(2_000), catalogue);
        let sold_train_id = purchase_train(&mut state, 1, ORIGIN).unwrap();
        state.player_company.funds = Money::ZERO;

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::Insolvent);
        assert!(evaluation.recovery_options.iter().any(|option| {
            matches!(option, RecoveryOption::SellAllAndRebuy {
                sold_train_ids,
                resale_proceeds,
                catalogue_index: 0,
                delivery_station_id: ORIGIN,
                journey,
            } if sold_train_ids == &vec![sold_train_id]
                && *resale_proceeds == Money::from_cents(1_400)
                && journey.origin_station_id == ORIGIN
                && journey.destination_station_id == DESTINATION
                && journey.operating_cost == Money::from_cents(150))
        }));
    }

    #[test]
    fn declares_bankruptcy_only_when_no_recovery_option_exists() {
        let mut state = configured_game(
            Money::from_cents(500),
            vec![diesel("Local", Money::from_cents(500))],
        );
        purchase_train(&mut state, 0, ORIGIN).unwrap();
        state.player_company.funds = Money::ZERO;

        let evaluation = evaluate_financial_recovery(&state).unwrap();

        assert_eq!(evaluation.status, FinancialStatus::Bankruptcy);
        assert!(evaluation.cash_only_options.is_empty());
        assert!(evaluation.recovery_options.is_empty());
    }
}

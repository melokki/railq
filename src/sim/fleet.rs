//! Fleet purchase and resale commands.
//!
//! A new Train is delivered READY to a selected Rail Station in the public
//! Rail Network. Once owned, its location only changes through a Journey.

use std::{error::Error, fmt};

use crate::{
    balance::DieselTrainCatalogueRecord,
    model::{CalculationError, GameState, Money, RailStationId, Train, TrainId, TrainStatus},
};

const RESALE_PERCENT: i64 = 70;
const PERCENT_DENOMINATOR: i64 = 100;

/// Why a Fleet purchase or resale cannot be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FleetError {
    /// The selected catalogue entry does not exist.
    CatalogueTrainNotFound { catalogue_index: usize },
    /// Delivery is only available to a Rail Station in the public Rail Network.
    InvalidDeliveryStation { station_id: RailStationId },
    /// Company Funds cannot cover the selected catalogue Train.
    InsufficientCompanyFunds { available: Money, required: Money },
    /// The selected catalogue entry has no valid purchase price.
    InvalidCataloguePrice { catalogue_index: usize },
    /// The selected Train is not owned by the Player Company.
    TrainNotFound { train_id: TrainId },
    /// A Train undertaking a Journey is unavailable for resale.
    TrainTravelling {
        train_id: TrainId,
        journey_id: crate::model::JourneyId,
    },
    /// An owned Train lacks a valid original purchase price.
    InvalidOriginalPurchasePrice { train_id: TrainId },
    /// Another Train ID cannot be represented.
    TrainIdExhausted,
    /// A checked money calculation could not be represented.
    Calculation(CalculationError),
}

impl fmt::Display for FleetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogueTrainNotFound { catalogue_index } => {
                write!(
                    formatter,
                    "catalogue Train {catalogue_index} does not exist"
                )
            }
            Self::InvalidDeliveryStation { station_id } => write!(
                formatter,
                "Rail Station {} is not available for Train delivery",
                station_id.get()
            ),
            Self::InsufficientCompanyFunds {
                available,
                required,
            } => write!(
                formatter,
                "Company Funds of {} cents cannot cover {} cents",
                available.cents(),
                required.cents()
            ),
            Self::InvalidCataloguePrice { catalogue_index } => write!(
                formatter,
                "catalogue Train {catalogue_index} has an invalid purchase price"
            ),
            Self::TrainNotFound { train_id } => {
                write!(formatter, "Train {} is not in the Fleet", train_id.get())
            }
            Self::TrainTravelling {
                train_id,
                journey_id,
            } => write!(
                formatter,
                "Train {} is travelling on Journey {} and cannot be sold",
                train_id.get(),
                journey_id.get()
            ),
            Self::InvalidOriginalPurchasePrice { train_id } => write!(
                formatter,
                "Train {} has an invalid original purchase price",
                train_id.get()
            ),
            Self::TrainIdExhausted => write!(formatter, "Train IDs are exhausted"),
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for FleetError {}

impl From<CalculationError> for FleetError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Purchases a catalogue Train and delivers it READY to a connected Rail Station.
///
/// Delivery has no fee. All validation and money calculations complete before
/// the Fleet or Company Funds are changed.
pub fn purchase_train(
    state: &mut GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
) -> Result<TrainId, FleetError> {
    let catalogue_train = state
        .rules
        .balance
        .diesel_catalogue()
        .get(catalogue_index)
        .ok_or(FleetError::CatalogueTrainNotFound { catalogue_index })?;
    let purchase_price = catalogue_train.purchase_price();
    if purchase_price.cents() <= 0 {
        return Err(FleetError::InvalidCataloguePrice { catalogue_index });
    }
    if !state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .any(|station| station.id == delivery_station_id)
    {
        return Err(FleetError::InvalidDeliveryStation {
            station_id: delivery_station_id,
        });
    }
    if state.player_company.funds < purchase_price {
        return Err(FleetError::InsufficientCompanyFunds {
            available: state.player_company.funds,
            required: purchase_price,
        });
    }

    let funds_after_purchase = state.player_company.funds.checked_sub(purchase_price)?;
    let train_id = next_train_id(&state.player_company.fleet.trains)?;
    let train = purchased_train(train_id, delivery_station_id, catalogue_train);

    state.player_company.funds = funds_after_purchase;
    state.player_company.fleet.trains.push(train);
    Ok(train_id)
}

/// Sells a READY Train for 70% of its original purchase price, rounded down.
///
/// A Train travelling on a Journey remains part of the Fleet and cannot be
/// sold until it arrives READY at a Rail Station.
pub fn sell_train(state: &mut GameState, train_id: TrainId) -> Result<Money, FleetError> {
    let train_index = state
        .player_company
        .fleet
        .trains
        .iter()
        .position(|train| train.id == train_id)
        .ok_or(FleetError::TrainNotFound { train_id })?;
    let train = &state.player_company.fleet.trains[train_index];
    if let TrainStatus::Travelling { journey_id } = train.status {
        return Err(FleetError::TrainTravelling {
            train_id,
            journey_id,
        });
    }

    let proceeds = resale_proceeds(train)?;
    let funds_after_sale = state.player_company.funds.checked_add(proceeds)?;

    state.player_company.funds = funds_after_sale;
    state.player_company.fleet.trains.remove(train_index);
    Ok(proceeds)
}

fn purchased_train(
    train_id: TrainId,
    delivery_station_id: RailStationId,
    catalogue_train: &DieselTrainCatalogueRecord,
) -> Train {
    Train {
        id: train_id,
        status: TrainStatus::Ready {
            at: delivery_station_id,
        },
        model_name: catalogue_train.name().to_owned(),
        original_purchase_price: catalogue_train.purchase_price(),
        passenger_capacity: catalogue_train.passenger_capacity(),
        speed: catalogue_train.speed(),
        fuel_cost_per_kilometre: catalogue_train.fuel_cost_per_kilometre(),
    }
}

fn next_train_id(trains: &[Train]) -> Result<TrainId, FleetError> {
    trains
        .iter()
        .map(|train| train.id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .map(TrainId::new)
        .ok_or(FleetError::TrainIdExhausted)
}

fn resale_proceeds(train: &Train) -> Result<Money, FleetError> {
    if train.original_purchase_price.cents() <= 0 {
        return Err(FleetError::InvalidOriginalPurchasePrice { train_id: train.id });
    }
    let cents = train
        .original_purchase_price
        .cents()
        .checked_mul(RESALE_PERCENT)
        .ok_or(CalculationError::Overflow {
            operation: "Train resale proceeds",
        })?
        / PERCENT_DENOMINATOR;
    Ok(Money::from_cents(cents))
}

#[cfg(test)]
mod tests {
    use crate::{
        balance::{BalanceConfig, DieselTrainCatalogueRecord},
        model::{
            MoneyPerKilometre, PassengerCapacity, SpeedMetresPerSecond, TrainStatus, UtcSeconds,
        },
        sim::world::create_new_game,
    };

    use super::*;

    fn game() -> GameState {
        create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0))
    }

    #[test]
    fn exact_cash_purchase_succeeds_and_delivers_a_ready_train() {
        let mut state = game();
        let catalogue_train = state.rules.balance.diesel_catalogue()[0].clone();
        state.player_company.funds = catalogue_train.purchase_price();

        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        assert_eq!(state.player_company.funds, Money::ZERO);
        assert_eq!(state.player_company.fleet.trains.len(), 1);
        assert_eq!(
            state.player_company.fleet.trains[0],
            Train {
                id: train_id,
                status: TrainStatus::Ready {
                    at: RailStationId::new(1)
                },
                model_name: catalogue_train.name().to_owned(),
                original_purchase_price: catalogue_train.purchase_price(),
                passenger_capacity: catalogue_train.passenger_capacity(),
                speed: catalogue_train.speed(),
                fuel_cost_per_kilometre: catalogue_train.fuel_cost_per_kilometre(),
            }
        );
    }

    #[test]
    fn insufficient_cash_leaves_the_game_unchanged() {
        let mut state = game();
        let purchase_price = state.rules.balance.diesel_catalogue()[0].purchase_price();
        state.player_company.funds = purchase_price.checked_sub(Money::from_cents(1)).unwrap();
        let unchanged = state.clone();

        assert_eq!(
            purchase_train(&mut state, 0, RailStationId::new(1)),
            Err(FleetError::InsufficientCompanyFunds {
                available: unchanged.player_company.funds,
                required: purchase_price,
            })
        );
        assert_eq!(state, unchanged);
    }

    #[test]
    fn invalid_delivery_leaves_the_game_unchanged() {
        let mut state = game();
        let unchanged = state.clone();
        let invalid_station_id = RailStationId::new(99);

        assert_eq!(
            purchase_train(&mut state, 0, invalid_station_id),
            Err(FleetError::InvalidDeliveryStation {
                station_id: invalid_station_id
            })
        );
        assert_eq!(state, unchanged);
    }

    #[test]
    fn resale_uses_the_original_price_and_rounds_down() {
        let mut state = game();
        let rate = MoneyPerKilometre::new(1).unwrap();
        state.rules.balance = BalanceConfig::new(
            rate,
            rate,
            Money::from_cents(101),
            vec![DieselTrainCatalogueRecord::new(
                "Test diesel",
                Money::from_cents(101),
                PassengerCapacity::new(1).unwrap(),
                SpeedMetresPerSecond::new(1).unwrap(),
                rate,
            )],
        );
        state.player_company.funds = Money::from_cents(101);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        state.rules.balance = BalanceConfig::new(
            rate,
            rate,
            Money::ZERO,
            vec![DieselTrainCatalogueRecord::new(
                "Retuned diesel",
                Money::from_cents(1_000),
                PassengerCapacity::new(2).unwrap(),
                SpeedMetresPerSecond::new(2).unwrap(),
                rate,
            )],
        );

        assert_eq!(sell_train(&mut state, train_id), Ok(Money::from_cents(70)));
        assert_eq!(state.player_company.funds, Money::from_cents(70));
        assert!(state.player_company.fleet.trains.is_empty());
    }

    #[test]
    fn travelling_train_cannot_be_sold_and_leaves_the_game_unchanged() {
        let mut state = game();
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
            journey_id: crate::model::JourneyId::new(7),
        };
        let unchanged = state.clone();

        assert_eq!(
            sell_train(&mut state, train_id),
            Err(FleetError::TrainTravelling {
                train_id,
                journey_id: crate::model::JourneyId::new(7),
            })
        );
        assert_eq!(state, unchanged);
    }
}

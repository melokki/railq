//! Fleet purchase and resale commands.
//!
//! A new Train is delivered READY to a selected Rail Station in the public
//! Rail Network. Once owned, its location only changes through a Journey.

use std::{error::Error, fmt};

use crate::{
    catalog::{TrainModel, train_catalogue},
    model::{
        CalculationError, EuropeanVehicleNumber, GameState, Money, RailStationId, Train, TrainId,
        TrainNickname, TrainStatus,
    },
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
    /// A permanent EVN cannot be allocated to the purchased Train.
    VehicleNumberUnavailable { train_id: TrainId },
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
            Self::VehicleNumberUnavailable { train_id } => write!(
                formatter,
                "Train {} cannot be assigned a valid European Vehicle Number",
                train_id.get()
            ),
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
    let catalogue_train = train_catalogue()
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
    let train_id = next_train_id(&state.player_company.fleet)?;
    let next_train_id = train_id
        .get()
        .checked_add(1)
        .ok_or(FleetError::TrainIdExhausted)?;
    let evn_unit_number = state
        .player_company
        .fleet
        .next_evn_unit_by_model
        .get(catalogue_train.id())
        .copied()
        .unwrap_or(1);
    let evn = EuropeanVehicleNumber::generate(
        catalogue_train.evn_type_code(),
        state.region.railway_registration.numeric_code,
        catalogue_train.evn_series_code(),
        evn_unit_number,
    )
    .map_err(|_| FleetError::VehicleNumberUnavailable { train_id })?;
    let next_evn_unit_number = evn_unit_number
        .checked_add(1)
        .ok_or(FleetError::VehicleNumberUnavailable { train_id })?;
    let train = purchased_train(train_id, evn, delivery_station_id, catalogue_train);

    state.player_company.funds = funds_after_purchase;
    state.player_company.fleet.next_train_id = next_train_id;
    state
        .player_company
        .fleet
        .next_evn_unit_by_model
        .insert(catalogue_train.id().clone(), next_evn_unit_number);
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

/// Changes or clears the player-facing nickname of one owned Train.
///
/// Renaming is metadata only: it is allowed while the Train is travelling and
/// never changes its EVN, model, Journey, or financial state.
pub fn rename_train(
    state: &mut GameState,
    train_id: TrainId,
    nickname: Option<TrainNickname>,
) -> Result<(), FleetError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter_mut()
        .find(|train| train.id == train_id)
        .ok_or(FleetError::TrainNotFound { train_id })?;
    train.nickname = nickname;
    Ok(())
}

fn purchased_train(
    train_id: TrainId,
    evn: EuropeanVehicleNumber,
    delivery_station_id: RailStationId,
    catalogue_train: &TrainModel,
) -> Train {
    Train {
        id: train_id,
        evn,
        nickname: None,
        status: TrainStatus::Ready {
            at: delivery_station_id,
        },
        model_id: catalogue_train.id().clone(),
        original_purchase_price: catalogue_train.purchase_price(),
    }
}

fn next_train_id(fleet: &crate::model::Fleet) -> Result<TrainId, FleetError> {
    let next = fleet.next_train_id;
    if next == 0 {
        return Err(FleetError::TrainIdExhausted);
    }
    Ok(TrainId::new(next))
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
        catalog::train_catalogue,
        model::{TrainModelId, TrainStatus, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::*;

    fn game() -> GameState {
        create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0))
    }

    #[test]
    fn exact_cash_purchase_succeeds_and_delivers_a_ready_train() {
        let mut state = game();
        let catalogue_train = train_catalogue().models()[0].clone();
        state.player_company.funds = catalogue_train.purchase_price();

        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        assert_eq!(state.player_company.funds, Money::ZERO);
        assert_eq!(state.player_company.fleet.trains.len(), 1);
        assert_eq!(state.player_company.fleet.next_train_id, train_id.get() + 1);
        assert_eq!(
            state
                .player_company
                .fleet
                .next_evn_unit_by_model
                .get(catalogue_train.id()),
            Some(&2)
        );
        assert_eq!(
            state.player_company.fleet.trains[0],
            Train {
                id: train_id,
                evn: EuropeanVehicleNumber::generate(
                    catalogue_train.evn_type_code(),
                    state.region.railway_registration.numeric_code,
                    catalogue_train.evn_series_code(),
                    1,
                )
                .unwrap(),
                nickname: None,
                status: TrainStatus::Ready {
                    at: RailStationId::new(1)
                },
                model_id: catalogue_train.id().clone(),
                original_purchase_price: catalogue_train.purchase_price(),
            }
        );
    }

    #[test]
    fn insufficient_cash_leaves_the_game_unchanged() {
        let mut state = game();
        let purchase_price = train_catalogue().models()[0].purchase_price();
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
    fn rename_changes_only_the_player_facing_nickname() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let station_id = state.region.rail_authority.rail_network.rail_stations[0].id;
        let train_id = purchase_train(&mut state, 0, station_id).unwrap();
        let original_evn = state.player_company.fleet.trains[0].evn.clone();

        rename_train(
            &mut state,
            train_id,
            Some(TrainNickname::parse("Little Runner").unwrap()),
        )
        .unwrap();

        let train = &state.player_company.fleet.trains[0];
        assert_eq!(train.nickname.as_ref().unwrap().as_str(), "Little Runner");
        assert_eq!(train.evn, original_evn);

        rename_train(&mut state, train_id, None).unwrap();
        assert!(state.player_company.fleet.trains[0].nickname.is_none());
    }

    #[test]
    fn resale_uses_the_original_price_and_rounds_down() {
        let mut state = game();
        let train_id = TrainId::new(1);
        state.player_company.fleet.trains.push(Train {
            id: train_id,
            evn: EuropeanVehicleNumber::generate(
                95,
                state.region.railway_registration.numeric_code,
                70,
                1,
            )
            .unwrap(),
            nickname: None,
            status: TrainStatus::Ready {
                at: RailStationId::new(1),
            },
            model_id: TrainModelId::new("local-70"),
            original_purchase_price: Money::from_cents(101),
        });
        state.player_company.fleet.next_train_id = train_id.get() + 1;
        state
            .player_company
            .fleet
            .next_evn_unit_by_model
            .insert(TrainModelId::new("local-70"), 2);
        state.player_company.funds = Money::ZERO;

        assert_eq!(sell_train(&mut state, train_id), Ok(Money::from_cents(70)));
        assert_eq!(state.player_company.funds, Money::from_cents(70));
        assert!(state.player_company.fleet.trains.is_empty());
    }

    #[test]
    fn train_ids_and_evns_are_not_reused_after_resale() {
        let mut state = game();
        state.player_company.funds = Money::from_cents(1_000_000);

        let first = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let first_evn = state.player_company.fleet.trains[0].evn.clone();
        sell_train(&mut state, first).unwrap();

        let second = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let second_evn = state.player_company.fleet.trains[0].evn.clone();

        assert!(second.get() > first.get());
        assert_ne!(first_evn, second_evn);
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

    #[test]
    fn resale_does_not_recycle_evn_unit_numbers() {
        let mut state = game();
        let catalogue_train = train_catalogue().models()[0].clone();
        state.player_company.funds = Money::from_cents(1_000_000);

        let first_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let first_evn = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == first_id)
            .unwrap()
            .evn
            .clone();
        sell_train(&mut state, first_id).unwrap();

        let second_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let second_evn = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == second_id)
            .unwrap()
            .evn
            .clone();

        assert_eq!(first_evn.series_code(), catalogue_train.evn_series_code());
        assert_eq!(first_evn.unit_number(), 1);
        assert_eq!(second_evn.series_code(), catalogue_train.evn_series_code());
        assert_eq!(second_evn.unit_number(), 2);
        assert_ne!(first_evn, second_evn);
        assert_eq!(
            state
                .player_company
                .fleet
                .next_evn_unit_by_model
                .get(catalogue_train.id()),
            Some(&3)
        );
    }


    #[test]
    fn each_model_allocates_its_own_evn_unit_sequence() {
        let mut state = game();
        state.player_company.funds = Money::from_cents(2_000_000);

        let first_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let second_id = purchase_train(&mut state, 1, RailStationId::new(1)).unwrap();

        let first = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == first_id)
            .unwrap();
        let second = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == second_id)
            .unwrap();

        assert_eq!(first.evn.series_code(), 70);
        assert_eq!(first.evn.unit_number(), 1);
        assert_eq!(second.evn.series_code(), 120);
        assert_eq!(second.evn.unit_number(), 1);
    }

}

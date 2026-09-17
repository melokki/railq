//! Compatibility decoder for RailQ's previous versioned RON save format.
//!
//! New saves are SQLite-backed. This module exists only so `SaveSlot::open_default`
//! can import the old `railq.ron` file when a player upgrades from the legacy
//! persistence format.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{SaveCodecError, catalogue_model_for_legacy_signature, validate_game_state};
use crate::{
    balance::BalanceConfig,
    model::{
        DemandRules, EuropeanVehicleNumber, Financials, Fleet, GameRules, GameState, Journey,
        JourneyPassengerGroup, Money, MoneyPerKilometre, OriginDestinationDemand,
        PassengerCapacity, PassengerService, PlayerCompany, RailLineId, RailStationId, Region,
        ServiceDirectionMode, ServiceId, SpeedMetresPerSecond, Train, TrainId, TrainModelId,
        TrainStatus, UtcSeconds,
        VehicleKeeperMark,
    },
    sim::world::railway_registration_for_existing_region,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySaveEnvelope<T> {
    version: u32,
    state: T,
}

#[derive(Deserialize, Serialize)]
struct LegacyGameStateV1 {
    world_seed: u64,
    region: Region,
    player_company: LegacyPlayerCompanyV1,
    origin_destination_demand: Vec<OriginDestinationDemand>,
    active_journeys: Vec<Journey>,
    financials: Financials,
    rules: LegacyGameRulesV1,
    last_processed_at: UtcSeconds,
}

#[derive(Deserialize, Serialize)]
struct LegacyPlayerCompanyV1 {
    name: String,
    funds: Money,
    fleet: LegacyFleetV1,
    passenger_services: Vec<LegacyPassengerServiceV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyFleetV1 {
    trains: Vec<LegacyTrainV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyTrainV1 {
    id: TrainId,
    status: TrainStatus,
    model_name: String,
    original_purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

#[derive(Deserialize, Serialize)]
struct LegacyPassengerServiceV1 {
    id: ServiceId,
    first_station_id: RailStationId,
    second_station_id: RailStationId,
    rail_line_ids: Vec<RailLineId>,
}

#[derive(Deserialize, Serialize)]
struct LegacyGameRulesV1 {
    balance: LegacyBalanceConfigV1,
    demand: DemandRules,
}

#[derive(Deserialize, Serialize)]
struct LegacyBalanceConfigV1 {
    fare_per_passenger_kilometre: MoneyPerKilometre,
    access_fee_per_train_kilometre: MoneyPerKilometre,
    starting_company_funds: Money,
    diesel_catalogue: Vec<LegacyDieselTrainCatalogueRecordV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyDieselTrainCatalogueRecordV1 {
    name: String,
    purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

pub(super) fn decode_legacy_game_state(source: &str) -> Result<GameState, SaveCodecError> {
    const LEGACY_RON_VERSION: u32 = 1;
    let envelope: LegacySaveEnvelope<LegacyGameStateV1> =
        ron::from_str(source).map_err(|error| SaveCodecError::LegacyDecode(error.to_string()))?;
    if envelope.version != LEGACY_RON_VERSION {
        return Err(SaveCodecError::UnsupportedVersion {
            found: envelope.version,
        });
    }

    let legacy = envelope.state;
    let mut region = legacy.region;
    region.railway_registration =
        railway_registration_for_existing_region(&region.name, legacy.world_seed);
    let mut next_evn_unit_by_model: BTreeMap<TrainModelId, u16> = BTreeMap::new();
    let trains = legacy
        .player_company
        .fleet
        .trains
        .into_iter()
        .map(|train| {
            let passenger_capacity = i64::from(train.passenger_capacity.passengers());
            let speed = i64::try_from(train.speed.metres_per_second()).map_err(|_| {
                SaveCodecError::InvalidValue {
                    field: "legacy Train speed",
                }
            })?;
            let fuel_rate = i64::try_from(train.fuel_cost_per_kilometre.cents_per_kilometre())
                .map_err(|_| SaveCodecError::InvalidValue {
                    field: "legacy Train fuel rate",
                })?;
            let model = catalogue_model_for_legacy_signature(
                &train.model_name,
                passenger_capacity,
                speed,
                fuel_rate,
            )
            .ok_or_else(|| SaveCodecError::TrainModelNotFound {
                model_name: train.model_name.clone(),
            })?;
            let unit_number = next_evn_unit_by_model
                .entry(model.id().clone())
                .or_insert(1);
            let evn = EuropeanVehicleNumber::generate(
                model.evn_type_code(),
                region.railway_registration.numeric_code,
                model.evn_series_code(),
                *unit_number,
            )
            .map_err(|_| SaveCodecError::InvalidValue {
                field: "European Vehicle Number",
            })?;
            let next_unit_number =
                (*unit_number)
                    .checked_add(1)
                    .ok_or(SaveCodecError::InvalidValue {
                        field: "EVN unit number",
                    })?;
            *unit_number = next_unit_number;
            Ok(Train {
                id: train.id,
                evn,
                nickname: None,
                status: train.status,
                model_id: model.id().clone(),
                original_purchase_price: train.original_purchase_price,
            })
        })
        .collect::<Result<Vec<_>, SaveCodecError>>()?;

    let passenger_services = legacy
        .player_company
        .passenger_services
        .into_iter()
        .enumerate()
        .map(|(index, service)| {
            let index = u32::try_from(index).map_err(|_| SaveCodecError::InvalidValue {
                field: "Passenger Service train number",
            })?;
            let forward_train_number = index
                .checked_mul(2)
                .and_then(|offset| 100_u32.checked_add(offset))
                .ok_or(SaveCodecError::InvalidValue {
                    field: "Passenger Service train number",
                })?;
            let reverse_train_number = forward_train_number
                .checked_add(1)
                .ok_or(SaveCodecError::InvalidValue {
                    field: "Passenger Service train number",
                })?;
            Ok(PassengerService {
                id: service.id,
                name: format!("R{}", service.id.get()),
                direction_mode: ServiceDirectionMode::BothDirections,
                forward_train_number,
                reverse_train_number: Some(reverse_train_number),
                stop_station_ids: vec![service.first_station_id, service.second_station_id],
                rail_line_ids: service.rail_line_ids,
            })
        })
        .collect::<Result<Vec<_>, SaveCodecError>>()?;
    let mut active_journeys = legacy.active_journeys;
    for journey in &mut active_journeys {
        if let Some(service) = passenger_services
            .iter()
            .find(|service| service.id == journey.service_id)
        {
            if journey.origin_station_id
                == service
                    .destination_station_id()
                    .unwrap_or(journey.origin_station_id)
                && journey.destination_station_id
                    == service
                        .origin_station_id()
                        .unwrap_or(journey.destination_station_id)
            {
                journey.current_stop_index = service.stop_station_ids.len().saturating_sub(1);
            }
        }
        if journey.passenger_groups.is_empty() && journey.passengers_carried > 0 {
            journey.passenger_groups.push(JourneyPassengerGroup {
                origin_station_id: journey.origin_station_id,
                destination_station_id: journey.destination_station_id,
                passengers: journey.passengers_carried,
                fare: journey.fare,
            });
        }
    }

    let state = GameState {
        world_seed: legacy.world_seed,
        region,
        player_company: PlayerCompany {
            vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name(
                &legacy.player_company.name,
            ),
            name: legacy.player_company.name,
            funds: legacy.player_company.funds,
            fleet: Fleet {
                next_train_display_number: trains
                    .iter()
                    .map(|train| train.id.get())
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1),
                trains,
                next_evn_unit_by_model,
            },
            passenger_services,
        },
        origin_destination_demand: legacy.origin_destination_demand,
        active_journeys,
        financials: legacy.financials,
        rules: GameRules {
            balance: BalanceConfig::new(
                legacy.rules.balance.fare_per_passenger_kilometre,
                legacy.rules.balance.access_fee_per_train_kilometre,
                legacy.rules.balance.starting_company_funds,
            ),
            demand: legacy.rules.demand,
        },
        last_processed_at: legacy.last_processed_at,
    };
    validate_game_state(&state).map_err(SaveCodecError::InvalidState)?;
    Ok(state)
}

#[cfg(test)]
pub(super) fn encode_v1_for_test(state: &GameState) -> String {
    use crate::catalog::{model_for_train, train_catalogue};

    let legacy_trains = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| {
            let model = model_for_train(train).expect("test Train model should exist");
            LegacyTrainV1 {
                id: train.id,
                status: train.status.clone(),
                model_name: model.name().to_owned(),
                original_purchase_price: train.original_purchase_price,
                passenger_capacity: model.passenger_capacity(),
                speed: model.speed(),
                fuel_cost_per_kilometre: model.fuel_cost_per_kilometre(),
            }
        })
        .collect();
    let legacy_catalogue = train_catalogue()
        .models()
        .iter()
        .map(|model| LegacyDieselTrainCatalogueRecordV1 {
            name: model.name().to_owned(),
            purchase_price: model.purchase_price(),
            passenger_capacity: model.passenger_capacity(),
            speed: model.speed(),
            fuel_cost_per_kilometre: model.fuel_cost_per_kilometre(),
        })
        .collect();
    let legacy = LegacyGameStateV1 {
        world_seed: state.world_seed,
        region: state.region.clone(),
        player_company: LegacyPlayerCompanyV1 {
            name: state.player_company.name.clone(),
            funds: state.player_company.funds,
            fleet: LegacyFleetV1 {
                trains: legacy_trains,
            },
            passenger_services: state
                .player_company
                .passenger_services
                .iter()
                .map(|service| LegacyPassengerServiceV1 {
                    id: service.id,
                    first_station_id: service.origin_station_id().unwrap(),
                    second_station_id: service.destination_station_id().unwrap(),
                    rail_line_ids: service.rail_line_ids.clone(),
                })
                .collect(),
        },
        origin_destination_demand: state.origin_destination_demand.clone(),
        active_journeys: state.active_journeys.clone(),
        financials: state.financials.clone(),
        rules: LegacyGameRulesV1 {
            balance: LegacyBalanceConfigV1 {
                fare_per_passenger_kilometre: state.rules.balance.fare_per_passenger_kilometre(),
                access_fee_per_train_kilometre: state
                    .rules
                    .balance
                    .access_fee_per_train_kilometre(),
                starting_company_funds: state.rules.balance.starting_company_funds(),
                diesel_catalogue: legacy_catalogue,
            },
            demand: state.rules.demand.clone(),
        },
        last_processed_at: state.last_processed_at,
    };

    ron::ser::to_string(&LegacySaveEnvelope {
        version: 1,
        state: legacy,
    })
    .expect("legacy test fixture should serialize")
}

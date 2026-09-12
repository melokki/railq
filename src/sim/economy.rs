//! Pure Journey economy calculations.
//!
//! A quote observes the current Player Company, Passenger Service, Rail
//! Network, and directional Passenger Demand without reserving passengers or
//! changing Company Funds. Manual dispatch applies the quoted effects later.

use std::{error::Error, fmt};

use crate::{
    catalog::model_for_train,
    model::{
        CalculationError, DistanceMetres, DurationSeconds, GameState, Money, RailLineId,
        RailStationId, ServiceId, TrainId, TrainStatus,
    },
};

/// The current economic and operational terms for one possible Journey.
///
/// `fare` is the automatic price for one passenger across the quoted distance.
/// `operating_revenue` is not available as Company Funds until the Journey
/// arrives; `cash_after_cost` only reflects the departure costs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JourneyQuote {
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub rail_line_path: Vec<RailLineId>,
    pub distance: DistanceMetres,
    pub boarded_passengers: u32,
    pub fare: Money,
    pub operating_revenue: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
    pub operating_cost: Money,
    pub journey_profitability: Money,
    pub duration: DurationSeconds,
    pub cash_after_cost: Money,
}

/// Why a Journey cannot be quoted from the current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EconomyError {
    /// The selected Train is not owned by the Player Company.
    TrainNotFound { train_id: TrainId },
    /// The selected Train references a model absent from the central catalogue.
    TrainModelNotFound { train_id: TrainId },
    /// The selected Passenger Service is not owned by the Player Company.
    ServiceNotFound { service_id: ServiceId },
    /// A Train travelling on a Journey cannot make another Journey quote.
    TrainTravelling { train_id: TrainId },
    /// A READY Train must be at the Passenger Service origin.
    TrainNotAtServiceOrigin {
        train_id: TrainId,
        station_id: RailStationId,
        service_id: ServiceId,
    },
    /// A Passenger Service with distinct endpoints must include a Rail Line.
    EmptyServicePath { service_id: ServiceId },
    /// A Passenger Service references a Rail Line absent from the Rail Network.
    RailLineNotFound { rail_line_id: RailLineId },
    /// The selected direction has no Waiting Passengers pool.
    DirectionalDemandNotFound {
        origin_station_id: RailStationId,
        destination_station_id: RailStationId,
    },
    /// A checked money, distance, or duration calculation could not be represented.
    Calculation(CalculationError),
}

impl fmt::Display for EconomyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TrainNotFound { train_id } => {
                write!(formatter, "Train {} is not in the Fleet", train_id.get())
            }
            Self::TrainModelNotFound { train_id } => write!(
                formatter,
                "Train {} references a model absent from the Train catalogue",
                train_id.get()
            ),
            Self::ServiceNotFound { service_id } => write!(
                formatter,
                "Passenger Service {} is not owned by the Player Company",
                service_id.get()
            ),
            Self::TrainTravelling { train_id } => write!(
                formatter,
                "Train {} is already travelling and cannot be quoted",
                train_id.get()
            ),
            Self::TrainNotAtServiceOrigin {
                train_id,
                station_id,
                service_id,
            } => write!(
                formatter,
                "Train {} at Rail Station {} is not at the origin of directional Passenger Service {}",
                train_id.get(),
                station_id.get(),
                service_id.get()
            ),
            Self::EmptyServicePath { service_id } => write!(
                formatter,
                "Passenger Service {} has no Rail Line path",
                service_id.get()
            ),
            Self::RailLineNotFound { rail_line_id } => write!(
                formatter,
                "Rail Line {} is not in the Rail Network",
                rail_line_id.get()
            ),
            Self::DirectionalDemandNotFound {
                origin_station_id,
                destination_station_id,
            } => write!(
                formatter,
                "no directional Passenger Demand exists from Rail Station {} to Rail Station {}",
                origin_station_id.get(),
                destination_station_id.get()
            ),
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for EconomyError {}

impl From<CalculationError> for EconomyError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Calculates a Journey quote without changing game state.
///
/// A Passenger Service runs only in its defined direction. The selected Train
/// must be READY at the Service origin, and only Waiting Passengers for that
/// exact origin-destination direction may board. A zero-passenger quote is
/// valid so a Player Company can pay to reposition a Train.
pub fn quote_journey(
    state: &GameState,
    train_id: TrainId,
    service_id: ServiceId,
) -> Result<JourneyQuote, EconomyError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or(EconomyError::TrainNotFound { train_id })?;
    let train_model =
        model_for_train(train).ok_or(EconomyError::TrainModelNotFound { train_id })?;
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .ok_or(EconomyError::ServiceNotFound { service_id })?;
    let origin_station_id = match train.status {
        TrainStatus::Ready { at } => at,
        TrainStatus::Travelling { .. } => return Err(EconomyError::TrainTravelling { train_id }),
    };
    let service_origin = service
        .origin_station_id()
        .ok_or(EconomyError::EmptyServicePath { service_id })?;
    let service_destination = service
        .destination_station_id()
        .ok_or(EconomyError::EmptyServicePath { service_id })?;
    if origin_station_id != service_origin {
        return Err(EconomyError::TrainNotAtServiceOrigin {
            train_id,
            station_id: origin_station_id,
            service_id,
        });
    }
    let destination_station_id = service_destination;
    let rail_line_path = service.rail_line_ids.clone();
    if rail_line_path.is_empty() {
        return Err(EconomyError::EmptyServicePath { service_id });
    }

    let distance_metres = rail_line_path
        .iter()
        .try_fold(0_u64, |total, rail_line_id| {
            let rail_line = state
                .region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .find(|line| line.id == *rail_line_id)
                .ok_or(EconomyError::RailLineNotFound {
                    rail_line_id: *rail_line_id,
                })?;
            total
                .checked_add(rail_line.distance.metres())
                .ok_or(CalculationError::Overflow {
                    operation: "Journey path distance",
                })
                .map_err(EconomyError::from)
        })?;
    let distance = DistanceMetres::new(i64::try_from(distance_metres).map_err(|_| {
        CalculationError::Overflow {
            operation: "Journey path distance",
        }
    })?)
    .map_err(|_| CalculationError::Overflow {
        operation: "Journey path distance",
    })?;
    let demand = state
        .origin_destination_demand
        .iter()
        .find(|demand| {
            demand.origin_station_id == origin_station_id
                && demand.destination_station_id == destination_station_id
        })
        .ok_or(EconomyError::DirectionalDemandNotFound {
            origin_station_id,
            destination_station_id,
        })?;
    let boarded_passengers = demand
        .waiting_passengers
        .min(train_model.passenger_capacity().passengers());
    let fare = state
        .rules
        .balance
        .fare_per_passenger_kilometre()
        .checked_charge(distance)?;
    let operating_revenue = fare.checked_mul(u64::from(boarded_passengers))?;
    let infrastructure_access_fee = state
        .rules
        .balance
        .access_fee_per_train_kilometre()
        .checked_charge(distance)?;
    let fuel_cost = train_model.fuel_cost_per_kilometre().checked_charge(distance)?;
    let operating_cost = infrastructure_access_fee.checked_add(fuel_cost)?;
    let journey_profitability = operating_revenue.checked_sub(operating_cost)?;
    let duration = distance.journey_duration(train_model.speed())?;
    let cash_after_cost = state.player_company.funds.checked_sub(operating_cost)?;

    Ok(JourneyQuote {
        service_id,
        train_id,
        origin_station_id,
        destination_station_id,
        rail_line_path,
        distance,
        boarded_passengers,
        fare,
        operating_revenue,
        infrastructure_access_fee,
        fuel_cost,
        operating_cost,
        journey_profitability,
        duration,
        cash_after_cost,
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        balance::BalanceConfig,
        model::{
            DemandRules, Financials, Fleet, GameRules, OriginDestinationDemand,
            PassengerArrivalRate, PassengerService, PlayerCompany, RailAuthority, RailLine,
            RailNetwork, RailStation, Settlement,
            Train, UtcSeconds,
        },
    };

    use super::*;

    const SERVICE_ID: ServiceId = ServiceId::new(1);
    const TRAIN_ID: TrainId = TrainId::new(1);
    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(3);
    const FIRST_LINE: RailLineId = RailLineId::new(1);
    const SECOND_LINE: RailLineId = RailLineId::new(2);

    fn fixture() -> GameState {
        let fare_rate = crate::model::MoneyPerKilometre::new(11).unwrap();
        let access_rate = crate::model::MoneyPerKilometre::new(5).unwrap();
        GameState {
            world_seed: 0,
            region: crate::model::Region {
                name: "Fixture Region".into(),
                population: 3,
                settlements: vec![
                    Settlement {
                        id: crate::model::SettlementId::new(1),
                        name: "Origin".into(),
                        population: 1,
                    },
                    Settlement {
                        id: crate::model::SettlementId::new(2),
                        name: "Middle".into(),
                        population: 1,
                    },
                    Settlement {
                        id: crate::model::SettlementId::new(3),
                        name: "Destination".into(),
                        population: 1,
                    },
                ],
                rail_authority: RailAuthority {
                    name: "Fixture Rail Authority".into(),
                    rail_network: RailNetwork {
                        rail_stations: vec![
                            RailStation {
                                id: ORIGIN,
                                settlement_id: crate::model::SettlementId::new(1),
                            },
                            RailStation {
                                id: RailStationId::new(2),
                                settlement_id: crate::model::SettlementId::new(2),
                            },
                            RailStation {
                                id: DESTINATION,
                                settlement_id: crate::model::SettlementId::new(3),
                            },
                        ],
                        rail_lines: vec![
                            RailLine {
                                id: FIRST_LINE,
                                first_station_id: ORIGIN,
                                second_station_id: RailStationId::new(2),
                                distance: DistanceMetres::new(1_000).unwrap(),
                            },
                            RailLine {
                                id: SECOND_LINE,
                                first_station_id: RailStationId::new(2),
                                second_station_id: DESTINATION,
                                distance: DistanceMetres::new(501).unwrap(),
                            },
                        ],
                    },
                },
            },
            player_company: PlayerCompany {
                name: "Fixture Passenger".into(),
                funds: Money::from_cents(10_000),
                fleet: Fleet {
                    trains: vec![Train {
                        id: TRAIN_ID,
                        status: TrainStatus::Ready { at: ORIGIN },
                        model_id: crate::model::TrainModelId::new("local-70"),
                        original_purchase_price: Money::from_cents(5_000),
                    }],
                },
                passenger_services: vec![PassengerService {
                    id: SERVICE_ID,
                    name: "R1".into(),
                    stop_station_ids: vec![ORIGIN, DESTINATION],
                    rail_line_ids: vec![FIRST_LINE, SECOND_LINE],
                }],
            },
            origin_destination_demand: vec![
                OriginDestinationDemand {
                    origin_station_id: ORIGIN,
                    destination_station_id: DESTINATION,
                    waiting_passengers: 3,
                    passenger_arrival_rate_per_hour: PassengerArrivalRate::new(1).unwrap(),
                    fractional_passenger_seconds: 0,
                },
                OriginDestinationDemand {
                    origin_station_id: DESTINATION,
                    destination_station_id: ORIGIN,
                    waiting_passengers: 1,
                    passenger_arrival_rate_per_hour: PassengerArrivalRate::new(1).unwrap(),
                    fractional_passenger_seconds: 0,
                },
            ],
            active_journeys: vec![],
            financials: Financials {
                operating_revenue: Money::ZERO,
                infrastructure_access_fees: Money::ZERO,
                fuel_costs: Money::ZERO,
                recent_journey_receipts: vec![],
            },
            rules: GameRules {
                balance: BalanceConfig::new(
                    fare_rate,
                    access_rate,
                    Money::from_cents(10_000),
                ),
                demand: DemandRules::provisional(),
            },
            last_processed_at: UtcSeconds::from_unix_seconds(0),
        }
    }

    #[test]
    fn known_fixture_independently_verifies_journey_economics_and_duration() {
        let quote = quote_journey(&fixture(), TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(quote.rail_line_path, [FIRST_LINE, SECOND_LINE]);
        assert_eq!(quote.distance, DistanceMetres::new(1_501).unwrap());
        assert_eq!(quote.boarded_passengers, 3);
        assert_eq!(quote.fare, Money::from_cents(17));
        assert_eq!(quote.operating_revenue, Money::from_cents(51));
        assert_eq!(quote.infrastructure_access_fee, Money::from_cents(8));
        assert_eq!(quote.fuel_cost, Money::from_cents(68));
        assert_eq!(quote.operating_cost, Money::from_cents(76));
        assert_eq!(quote.journey_profitability, Money::from_cents(-25));
        assert_eq!(quote.duration, DurationSeconds::from_seconds(61));
        assert_eq!(quote.cash_after_cost, Money::from_cents(9_924));
    }

    #[test]
    fn passenger_capacity_limits_boarding() {
        let mut state = fixture();
        state.origin_destination_demand[0].waiting_passengers = 80;

        assert_eq!(
            quote_journey(&state, TRAIN_ID, SERVICE_ID)
                .unwrap()
                .boarded_passengers,
            70
        );
    }

    #[test]
    fn directional_service_rejects_a_train_waiting_at_its_destination() {
        let mut state = fixture();
        state.player_company.fleet.trains[0].status = TrainStatus::Ready { at: DESTINATION };

        assert_eq!(
            quote_journey(&state, TRAIN_ID, SERVICE_ID),
            Err(EconomyError::TrainNotAtServiceOrigin {
                train_id: TRAIN_ID,
                station_id: DESTINATION,
                service_id: SERVICE_ID,
            })
        );
    }

    #[test]
    fn quoting_does_not_mutate_game_state() {
        let state = fixture();
        let before = state.clone();

        let _quote = quote_journey(&state, TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(state, before);
    }
}

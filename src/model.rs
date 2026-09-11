//! Core domain value types.
//!
//! All calculations use integer units. When a calculation has a fractional
//! result, RailQ rounds up: a partial cent or partial second is charged or
//! scheduled as one whole unit. This keeps quotes deterministic and never
//! understates a cost or Journey duration.

use std::{error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::balance::BalanceConfig;

/// An input that violates a value object's invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// The value must be greater than zero.
    NonPositive { unit: &'static str },
    /// The value cannot fit the storage unit for this value object.
    OutOfRange { unit: &'static str },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositive { unit } => write!(formatter, "{unit} must be greater than zero"),
            Self::OutOfRange { unit } => write!(formatter, "{unit} is outside the supported range"),
        }
    }
}

impl Error for ValidationError {}

/// A checked calculation that cannot be represented in its result unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalculationError {
    Overflow { operation: &'static str },
}

impl fmt::Display for CalculationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow { operation } => {
                write!(formatter, "overflow while calculating {operation}")
            }
        }
    }
}

impl Error for CalculationError {}

macro_rules! domain_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name(u64);

        impl $name {
            /// Creates an ID allocated by its owning collection.
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Returns the stored identifier value.
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

domain_id!(SettlementId, "The identity of a Settlement in a Region.");
domain_id!(
    RailStationId,
    "The identity of a Rail Station in the Rail Network."
);
domain_id!(
    RailLineId,
    "The identity of a Rail Line in the Rail Network."
);
domain_id!(
    TrainId,
    "The identity of a Train owned by the Player Company."
);
domain_id!(
    ServiceId,
    "The identity of a Passenger Service owned by the Player Company."
);
domain_id!(JourneyId, "The identity of one physical Train movement.");

/// A signed amount of money stored exactly as integer cents.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);

    /// Creates a money amount from its exact cent value.
    pub const fn from_cents(cents: i64) -> Self {
        Self(cents)
    }

    /// Returns the exact cent value.
    pub const fn cents(self) -> i64 {
        self.0
    }

    /// Adds two money amounts without silently wrapping.
    pub fn checked_add(self, other: Self) -> Result<Self, CalculationError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money addition",
            })
    }

    /// Subtracts two money amounts without silently wrapping.
    pub fn checked_sub(self, other: Self) -> Result<Self, CalculationError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money subtraction",
            })
    }

    /// Multiplies a money amount by a count without silently wrapping.
    pub fn checked_mul(self, count: u64) -> Result<Self, CalculationError> {
        let count = i64::try_from(count).map_err(|_| CalculationError::Overflow {
            operation: "money multiplication",
        })?;
        self.0
            .checked_mul(count)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money multiplication",
            })
    }
}

/// A positive passenger capacity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PassengerCapacity(u32);

impl PassengerCapacity {
    pub fn new(passengers: i64) -> Result<Self, ValidationError> {
        let passengers = u32::try_from(passengers).map_err(|_| ValidationError::OutOfRange {
            unit: "passenger capacity",
        })?;
        if passengers == 0 {
            return Err(ValidationError::NonPositive {
                unit: "passenger capacity",
            });
        }
        Ok(Self(passengers))
    }

    pub const fn passengers(self) -> u32 {
        self.0
    }
}

/// A positive Train speed stored as metres per second.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SpeedMetresPerSecond(u64);

impl SpeedMetresPerSecond {
    pub fn new(metres_per_second: i64) -> Result<Self, ValidationError> {
        let metres_per_second =
            u64::try_from(metres_per_second).map_err(|_| ValidationError::OutOfRange {
                unit: "speed in metres per second",
            })?;
        if metres_per_second == 0 {
            return Err(ValidationError::NonPositive {
                unit: "speed in metres per second",
            });
        }
        Ok(Self(metres_per_second))
    }

    pub const fn metres_per_second(self) -> u64 {
        self.0
    }
}

/// A positive physical distance stored as integer metres.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DistanceMetres(u64);

impl DistanceMetres {
    pub fn new(metres: i64) -> Result<Self, ValidationError> {
        let metres = u64::try_from(metres).map_err(|_| ValidationError::OutOfRange {
            unit: "distance in metres",
        })?;
        if metres == 0 {
            return Err(ValidationError::NonPositive {
                unit: "distance in metres",
            });
        }
        Ok(Self(metres))
    }

    pub const fn metres(self) -> u64 {
        self.0
    }

    /// Calculates a Journey duration, rounding a partial second up.
    pub fn journey_duration(
        self,
        speed: SpeedMetresPerSecond,
    ) -> Result<DurationSeconds, CalculationError> {
        let seconds = self.0 / speed.0;
        let has_fractional_second = self.0 % speed.0 != 0;
        let seconds = if has_fractional_second {
            seconds.checked_add(1).ok_or(CalculationError::Overflow {
                operation: "journey duration",
            })?
        } else {
            seconds
        };
        Ok(DurationSeconds(seconds))
    }
}

/// A non-negative elapsed duration stored as whole seconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DurationSeconds(u64);

impl DurationSeconds {
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    pub const fn seconds(self) -> u64 {
        self.0
    }
}

/// A UTC Unix timestamp stored as whole seconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct UtcSeconds(i64);

impl UtcSeconds {
    pub const fn from_unix_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    pub const fn unix_seconds(self) -> i64 {
        self.0
    }

    /// Advances a timestamp without silently wrapping.
    pub fn checked_add(self, duration: DurationSeconds) -> Result<Self, CalculationError> {
        let duration = i64::try_from(duration.0).map_err(|_| CalculationError::Overflow {
            operation: "UTC timestamp addition",
        })?;
        self.0
            .checked_add(duration)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "UTC timestamp addition",
            })
    }
}

/// A positive monetary rate in cents per kilometre.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MoneyPerKilometre(u64);

impl MoneyPerKilometre {
    pub fn new(cents_per_kilometre: i64) -> Result<Self, ValidationError> {
        let cents_per_kilometre =
            u64::try_from(cents_per_kilometre).map_err(|_| ValidationError::OutOfRange {
                unit: "money rate in cents per kilometre",
            })?;
        if cents_per_kilometre == 0 {
            return Err(ValidationError::NonPositive {
                unit: "money rate in cents per kilometre",
            });
        }
        Ok(Self(cents_per_kilometre))
    }

    pub const fn cents_per_kilometre(self) -> u64 {
        self.0
    }

    /// Charges for a distance, rounding a partial cent up.
    pub fn checked_charge(self, distance: DistanceMetres) -> Result<Money, CalculationError> {
        const METRES_PER_KILOMETRE: u64 = 1_000;

        let cent_metres = self
            .0
            .checked_mul(distance.0)
            .ok_or(CalculationError::Overflow {
                operation: "distance rate",
            })?;
        let cents = cent_metres / METRES_PER_KILOMETRE;
        let cents = if cent_metres % METRES_PER_KILOMETRE != 0 {
            cents.checked_add(1).ok_or(CalculationError::Overflow {
                operation: "distance rate rounding",
            })?
        } else {
            cents
        };
        let cents = i64::try_from(cents).map_err(|_| CalculationError::Overflow {
            operation: "distance rate money conversion",
        })?;
        Ok(Money::from_cents(cents))
    }
}

macro_rules! deserialize_validated_positive {
    ($name:ident, $primitive:ty) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = <$primitive>::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

deserialize_validated_positive!(PassengerCapacity, i64);
deserialize_validated_positive!(SpeedMetresPerSecond, i64);
deserialize_validated_positive!(DistanceMetres, i64);
deserialize_validated_positive!(MoneyPerKilometre, i64);
deserialize_validated_positive!(PassengerArrivalRate, i64);

/// The complete mutable state of one RailQ game.
///
/// The Region owns public infrastructure through its Rail Authority. The
/// Player Company separately owns its Fleet and Passenger Services. Active
/// Journeys and origin-destination demand belong to the game because they
/// describe the current operating state rather than either owner's assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameState {
    /// The seed that generated this game's Region. It is saved so a loaded
    /// game never needs to regenerate its world.
    pub world_seed: u64,
    pub region: Region,
    pub player_company: PlayerCompany,
    pub origin_destination_demand: Vec<OriginDestinationDemand>,
    pub active_journeys: Vec<Journey>,
    pub financials: Financials,
    pub rules: GameRules,
    pub last_processed_at: UtcSeconds,
}

/// The fictional place in which a game takes place.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Region {
    pub name: String,
    /// The total Population of every Settlement in this Region.
    pub population: u64,
    pub settlements: Vec<Settlement>,
    pub rail_authority: RailAuthority,
}

/// A populated place in a Region, with or without railway access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settlement {
    pub id: SettlementId,
    pub name: String,
    pub population: u64,
}

/// The public owner of a Region's Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailAuthority {
    pub name: String,
    pub rail_network: RailNetwork,
}

/// The physical infrastructure owned by a Rail Authority.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailNetwork {
    pub rail_stations: Vec<RailStation>,
    pub rail_lines: Vec<RailLine>,
}

/// A facility providing one Settlement access to the Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailStation {
    pub id: RailStationId,
    pub settlement_id: SettlementId,
}

/// A physical connection between two Rail Stations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailLine {
    pub id: RailLineId,
    pub first_station_id: RailStationId,
    pub second_station_id: RailStationId,
    pub distance: DistanceMetres,
}

/// The passenger railway company controlled by the player.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlayerCompany {
    pub name: String,
    pub funds: Money,
    pub fleet: Fleet,
    pub passenger_services: Vec<PassengerService>,
}

/// All Trains owned by the Player Company.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Fleet {
    pub trains: Vec<Train>,
}

/// Passenger rolling stock owned by the Player Company.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Train {
    pub id: TrainId,
    pub status: TrainStatus,
    /// The catalogue name captured when this Train joined the Fleet.
    pub model_name: String,
    /// The amount paid when this Train was purchased.
    ///
    /// Resale is calculated from this original price, not a later catalogue
    /// price.
    pub original_purchase_price: Money,
    pub passenger_capacity: PassengerCapacity,
    pub speed: SpeedMetresPerSecond,
    pub fuel_cost_per_kilometre: MoneyPerKilometre,
}

/// The mutually exclusive operating status of a Train.
///
/// A Train is either ready at a Rail Station or travelling on one Journey;
/// the enum representation makes it impossible to represent both at once.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TrainStatus {
    Ready { at: RailStationId },
    Travelling { journey_id: JourneyId },
}

/// A persistent commercial offering over ordered Rail Lines.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PassengerService {
    pub id: ServiceId,
    pub first_station_id: RailStationId,
    pub second_station_id: RailStationId,
    pub rail_line_ids: Vec<RailLineId>,
}

/// One physical movement of a Train under a Passenger Service.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Journey {
    pub id: JourneyId,
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub passengers_carried: u32,
    pub departed_at: UtcSeconds,
    pub arrives_at: UtcSeconds,
}

/// Waiting passengers for one directional origin-destination market.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OriginDestinationDemand {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub waiting_passengers: u32,
    /// New Waiting Passengers generated per hour for this direction.
    pub passenger_arrival_rate_per_hour: PassengerArrivalRate,
    /// Passenger-seconds left over after the last whole-passenger update.
    ///
    /// This is always less than one hour while the pool is below its cap.
    /// It is cleared when the pool reaches the cap, so capped demand cannot
    /// become a hidden backlog.
    pub fractional_passenger_seconds: u64,
}

/// A positive directional Passenger Demand rate, in passengers per hour.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PassengerArrivalRate(u32);

impl PassengerArrivalRate {
    pub fn new(passengers_per_hour: i64) -> Result<Self, ValidationError> {
        let passengers_per_hour =
            u32::try_from(passengers_per_hour).map_err(|_| ValidationError::OutOfRange {
                unit: "passenger arrival rate per hour",
            })?;
        if passengers_per_hour == 0 {
            return Err(ValidationError::NonPositive {
                unit: "passenger arrival rate per hour",
            });
        }
        Ok(Self(passengers_per_hour))
    }

    pub const fn passengers_per_hour(self) -> u32 {
        self.0
    }
}

/// Cumulative financial data and receipts for the current game.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Financials {
    pub operating_revenue: Money,
    pub infrastructure_access_fees: Money,
    pub fuel_costs: Money,
    pub recent_journey_receipts: Vec<JourneyReceipt>,
}

/// The settled financial result of one Journey.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JourneyReceipt {
    pub journey_id: JourneyId,
    pub revenue: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
}

/// Rules saved with a game so its economics do not change after a balance update.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameRules {
    pub balance: BalanceConfig,
    pub demand: DemandRules,
}

/// Tunable Passenger Demand rules saved with a game.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DemandRules {
    /// The time period of demand a directional pool can retain.
    pub cap_duration: DurationSeconds,
}

impl DemandRules {
    /// The initial playtest rule retains at most 24 hours of each directional
    /// Passenger Demand rate.
    pub const fn provisional() -> Self {
        Self {
            cap_duration: DurationSeconds::from_seconds(24 * 60 * 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{any::TypeId, collections::HashSet};

    use super::*;

    #[test]
    fn domain_ids_are_distinct_value_types() {
        let ids = [
            TypeId::of::<SettlementId>(),
            TypeId::of::<RailStationId>(),
            TypeId::of::<RailLineId>(),
            TypeId::of::<TrainId>(),
            TypeId::of::<ServiceId>(),
            TypeId::of::<JourneyId>(),
        ];
        assert_eq!(HashSet::from(ids).len(), ids.len());

        assert_eq!(SettlementId::new(7).get(), 7);
        assert_eq!(RailStationId::new(7).get(), 7);
        assert_eq!(RailLineId::new(7).get(), 7);
        assert_eq!(TrainId::new(7).get(), 7);
        assert_eq!(ServiceId::new(7).get(), 7);
        assert_eq!(JourneyId::new(7).get(), 7);
    }

    #[test]
    fn rejects_non_positive_domain_values() {
        for value in [-1, 0] {
            assert!(matches!(
                PassengerCapacity::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                SpeedMetresPerSecond::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                DistanceMetres::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                MoneyPerKilometre::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
        }
    }

    #[test]
    fn rounds_partial_seconds_and_cents_up() {
        let speed = SpeedMetresPerSecond::new(1_000).unwrap();
        assert_eq!(
            DistanceMetres::new(1_000)
                .unwrap()
                .journey_duration(speed)
                .unwrap(),
            DurationSeconds::from_seconds(1)
        );
        assert_eq!(
            DistanceMetres::new(1_001)
                .unwrap()
                .journey_duration(speed)
                .unwrap(),
            DurationSeconds::from_seconds(2)
        );

        let rate = MoneyPerKilometre::new(1).unwrap();
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1_000).unwrap())
                .unwrap(),
            Money::from_cents(1)
        );
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1).unwrap())
                .unwrap(),
            Money::from_cents(1)
        );
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1_001).unwrap())
                .unwrap(),
            Money::from_cents(2)
        );
    }

    #[test]
    fn reports_overflow_boundaries() {
        assert_eq!(
            Money::from_cents(i64::MAX).checked_add(Money::from_cents(1)),
            Err(CalculationError::Overflow {
                operation: "money addition"
            })
        );
        assert_eq!(
            Money::from_cents(i64::MIN).checked_sub(Money::from_cents(1)),
            Err(CalculationError::Overflow {
                operation: "money subtraction"
            })
        );
        assert_eq!(
            MoneyPerKilometre::new(i64::MAX)
                .unwrap()
                .checked_charge(DistanceMetres::new(i64::MAX).unwrap()),
            Err(CalculationError::Overflow {
                operation: "distance rate"
            })
        );
        assert_eq!(
            UtcSeconds::from_unix_seconds(i64::MAX).checked_add(DurationSeconds::from_seconds(1)),
            Err(CalculationError::Overflow {
                operation: "UTC timestamp addition"
            })
        );
    }

    #[test]
    fn a_small_valid_game_state_fixture_builds() {
        let settlement_id = SettlementId::new(1);
        let station_id = RailStationId::new(1);
        let train_id = TrainId::new(1);
        let rate = MoneyPerKilometre::new(1).unwrap();
        let state = GameState {
            world_seed: 0,
            region: Region {
                name: "Varelia".into(),
                population: 1_000,
                settlements: vec![Settlement {
                    id: settlement_id,
                    name: "Alden".into(),
                    population: 1_000,
                }],
                rail_authority: RailAuthority {
                    name: "Varelia Rail Authority".into(),
                    rail_network: RailNetwork {
                        rail_stations: vec![RailStation {
                            id: station_id,
                            settlement_id,
                        }],
                        rail_lines: vec![],
                    },
                },
            },
            player_company: PlayerCompany {
                name: "Alden Passenger".into(),
                funds: Money::from_cents(10_000),
                fleet: Fleet {
                    trains: vec![Train {
                        id: train_id,
                        status: TrainStatus::Ready { at: station_id },
                        model_name: "Test diesel".into(),
                        original_purchase_price: Money::from_cents(5_000),
                        passenger_capacity: PassengerCapacity::new(40).unwrap(),
                        speed: SpeedMetresPerSecond::new(20).unwrap(),
                        fuel_cost_per_kilometre: rate,
                    }],
                },
                passenger_services: vec![],
            },
            origin_destination_demand: vec![],
            active_journeys: vec![],
            financials: Financials {
                operating_revenue: Money::ZERO,
                infrastructure_access_fees: Money::ZERO,
                fuel_costs: Money::ZERO,
                recent_journey_receipts: vec![],
            },
            rules: GameRules {
                balance: BalanceConfig::new(rate, rate, Money::from_cents(10_000), vec![]),
                demand: DemandRules::provisional(),
            },
            last_processed_at: UtcSeconds::from_unix_seconds(0),
        };

        assert_eq!(state.player_company.fleet.trains[0].id, train_id);
        assert_eq!(
            state.region.rail_authority.rail_network.rail_stations[0].id,
            station_id
        );
    }

    #[test]
    fn rail_authority_and_player_company_are_distinct_owners() {
        fn owns_network(_: &RailAuthority) {}
        fn owns_fleet_and_services(_: &PlayerCompany) {}

        let authority = RailAuthority {
            name: "Varelia Rail Authority".into(),
            rail_network: RailNetwork::default(),
        };
        let company = PlayerCompany {
            name: "Alden Passenger".into(),
            funds: Money::ZERO,
            fleet: Fleet::default(),
            passenger_services: vec![],
        };

        owns_network(&authority);
        owns_fleet_and_services(&company);
    }

    #[test]
    fn a_train_has_exactly_one_operating_status() {
        let station_id = RailStationId::new(1);
        let ready = TrainStatus::Ready { at: station_id };
        let travelling = TrainStatus::Travelling {
            journey_id: JourneyId::new(1),
        };

        assert!(matches!(ready, TrainStatus::Ready { .. }));
        assert!(matches!(travelling, TrainStatus::Travelling { .. }));
    }

    #[test]
    fn rejects_non_positive_passenger_arrival_rates() {
        for rate in [-1, 0] {
            assert!(PassengerArrivalRate::new(rate).is_err());
        }
    }
}

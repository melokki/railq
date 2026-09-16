//! Passenger-service, Journey, and origin-destination demand domain types.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{
    JourneyId, Money, RailLineId, RailStationId, ServiceId, TrainId, UtcSeconds, ValidationError,
};

/// A directional passenger offering over an ordered set of stops and Rail Lines.
///
/// `stop_station_ids` contains only the stations at which the Service calls.
/// `rail_line_ids` contains the full physical path between those stops, so a
/// Service may pass through intermediate Rail Stations without stopping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PassengerService {
    pub id: ServiceId,
    pub name: String,
    pub stop_station_ids: Vec<RailStationId>,
    pub rail_line_ids: Vec<RailLineId>,
}

impl PassengerService {
    /// Returns the directional origin of this Service.
    pub fn origin_station_id(&self) -> Option<RailStationId> {
        self.stop_station_ids.first().copied()
    }

    /// Returns the directional destination of this Service.
    pub fn destination_station_id(&self) -> Option<RailStationId> {
        self.stop_station_ids.last().copied()
    }
}

/// Passengers currently aboard one active Journey, grouped by their final stop.
///
/// A group keeps its boarding origin and accepted fare so revenue can be
/// credited when those passengers actually alight, including after one or
/// more intermediate Service stops.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JourneyPassengerGroup {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub passengers: u32,
    pub fare: Money,
}

/// One Train run over a directional Passenger Service.
///
/// `current_stop_index` identifies the Service stop from which the current leg
/// departed. `arrives_at` is therefore the ETA of the next Service stop, not
/// necessarily the Service terminus. The same Journey ID remains active while
/// the Train calls at intermediate stops.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Journey {
    pub id: JourneyId,
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    /// Total passenger boardings across the Service run so far.
    pub passengers_carried: u32,
    /// Through fare from the Service origin to terminus. Kept as a stable
    /// summary value; individual onboard groups may have shorter fares.
    pub fare: Money,
    /// Total booked revenue for all passenger groups boarded so far.
    pub operating_revenue: Money,
    /// Revenue already credited because passengers have reached their stops.
    #[serde(default)]
    pub credited_revenue: Money,
    /// The Rail Authority infrastructure charge for the complete Service run,
    /// paid at initial dispatch.
    pub infrastructure_access_fee: Money,
    /// Diesel fuel cost for the complete Service run, paid at initial dispatch.
    pub fuel_cost: Money,
    /// Index of the Service stop at which the current leg began.
    #[serde(default)]
    pub current_stop_index: usize,
    /// Passenger groups still aboard the Train.
    #[serde(default)]
    pub passenger_groups: Vec<JourneyPassengerGroup>,
    /// Departure time of the current leg.
    pub departed_at: UtcSeconds,
    /// Arrival time of the next Service stop.
    pub arrives_at: UtcSeconds,
}

impl Journey {
    /// Current onboard occupancy. This may be lower than `passengers_carried`
    /// because the latter counts every boarding over the full Service run.
    pub fn onboard_passengers(&self) -> u32 {
        self.passenger_groups
            .iter()
            .fold(0_u32, |total, group| total.saturating_add(group.passengers))
    }
}

/// Waiting passengers for one directional origin-destination market.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OriginDestinationDemand {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub waiting_passengers: u32,
    /// Long-term rail adoption for this directional market.
    ///
    /// This is persisted independently from the seeded potential arrival rate
    /// so later simulation batches can grow demand through actual operation.
    #[serde(default = "MarketMaturity::full")]
    pub market_maturity: MarketMaturity,
    /// Seeded/base Passenger Demand potential for this direction.
    ///
    /// Market maturity is persisted separately and scales this base rate in
    /// the Passenger Demand simulation.
    pub passenger_arrival_rate_per_hour: PassengerArrivalRate,
    /// Passenger-seconds left over after the last whole-passenger update.
    ///
    /// This is always less than one hour while the pool is below its cap.
    /// It is cleared when the pool reaches the cap, so capped demand cannot
    /// become a hidden backlog.
    pub fractional_passenger_seconds: u64,
}

/// Rail-adoption maturity for one directional passenger market, expressed in
/// basis points so growth can remain gradual without floating-point state.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MarketMaturity(u16);

impl MarketMaturity {
    pub const FULL_BASIS_POINTS: u16 = 10_000;

    pub fn from_basis_points(basis_points: i64) -> Result<Self, ValidationError> {
        let basis_points =
            u16::try_from(basis_points).map_err(|_| ValidationError::OutOfRange {
                unit: "market maturity basis points",
            })?;
        if basis_points > Self::FULL_BASIS_POINTS {
            return Err(ValidationError::OutOfRange {
                unit: "market maturity basis points",
            });
        }
        Ok(Self(basis_points))
    }

    pub const fn full() -> Self {
        Self(Self::FULL_BASIS_POINTS)
    }

    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

impl Serialize for MarketMaturity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MarketMaturity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)?;
        Self::from_basis_points(value).map_err(de::Error::custom)
    }
}

/// A positive directional Passenger Demand rate, in passengers per hour.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

impl Serialize for PassengerArrivalRate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        i64::from(self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PassengerArrivalRate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

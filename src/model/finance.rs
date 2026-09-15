use serde::{Deserialize, Serialize};

use crate::balance::BalanceConfig;

use super::{
    DurationSeconds, JourneyId, Money, RailStationId, TrainId, UtcSeconds,
};

/// Cumulative financial data and receipts for the current game.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Financials {
    pub operating_revenue: Money,
    pub infrastructure_access_fees: Money,
    pub fuel_costs: Money,
    pub recent_journey_receipts: Vec<JourneyReceipt>,
}

/// The settled financial result and operating context of one Journey.
///
/// The optional context fields were added after the initial save format. They
/// default to `None` so version-1 RON saves containing older receipts remain
/// readable; newly settled Journeys always populate the complete snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JourneyReceipt {
    pub journey_id: JourneyId,
    pub revenue: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
    #[serde(default)]
    pub train_id: Option<TrainId>,
    #[serde(default)]
    pub train_model_name: Option<String>,
    #[serde(default)]
    pub origin_station_id: Option<RailStationId>,
    #[serde(default)]
    pub destination_station_id: Option<RailStationId>,
    #[serde(default)]
    pub passengers_carried: Option<u32>,
    #[serde(default)]
    pub passenger_capacity: Option<u32>,
    #[serde(default)]
    pub completed_at: Option<UtcSeconds>,
}

/// Per-save simulation rules. Static Train model definitions are build content
/// from the central catalogue and are intentionally not duplicated here.
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

use serde::{Deserialize, Serialize};

use crate::balance::BalanceConfig;

use super::{DurationSeconds, JourneyId, Money, RailStationId, TrainId, UtcSeconds};

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
    #[serde(default)]
    pub authority: AuthorityRules,
}

/// Tunable Rail Authority progression cadence saved with a game.
///
/// Planning and construction consume these values directly, keeping request
/// cadence, review throughput, mobilisation, and build duration deterministic
/// for each save.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorityRules {
    request_queue_delay: DurationSeconds,
    review_duration: DurationSeconds,
    proposal_duration: DurationSeconds,
    council_request_cooldown: DurationSeconds,
    deferred_reconsideration_delay: DurationSeconds,
    construction_mobilisation_delay: DurationSeconds,
    new_line_base_construction_duration: DurationSeconds,
    low_difficulty_seconds_per_kilometre: u64,
    moderate_difficulty_seconds_per_kilometre: u64,
    high_difficulty_seconds_per_kilometre: u64,
    max_active_expansion_projects: u32,
}

impl AuthorityRules {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        request_queue_delay: DurationSeconds,
        review_duration: DurationSeconds,
        proposal_duration: DurationSeconds,
        council_request_cooldown: DurationSeconds,
        deferred_reconsideration_delay: DurationSeconds,
        construction_mobilisation_delay: DurationSeconds,
        new_line_base_construction_duration: DurationSeconds,
        low_difficulty_seconds_per_kilometre: u64,
        moderate_difficulty_seconds_per_kilometre: u64,
        high_difficulty_seconds_per_kilometre: u64,
        max_active_expansion_projects: u32,
    ) -> Self {
        Self {
            request_queue_delay,
            review_duration,
            proposal_duration,
            council_request_cooldown,
            deferred_reconsideration_delay,
            construction_mobilisation_delay,
            new_line_base_construction_duration,
            low_difficulty_seconds_per_kilometre,
            moderate_difficulty_seconds_per_kilometre,
            high_difficulty_seconds_per_kilometre,
            max_active_expansion_projects,
        }
    }

    /// Initial slower Authority pacing derived from the first progression playtest.
    ///
    /// These values are persisted with both migrated and newly created saves so
    /// progression does not change just because a build ships new defaults.
    pub const fn provisional() -> Self {
        Self::new(
            DurationSeconds::from_seconds(60 * 60),
            DurationSeconds::from_seconds(2 * 60 * 60),
            DurationSeconds::from_seconds(60 * 60),
            DurationSeconds::from_seconds(24 * 60 * 60),
            DurationSeconds::from_seconds(24 * 60 * 60),
            DurationSeconds::from_seconds(60 * 60),
            DurationSeconds::from_seconds(5 * 60 * 60),
            2 * 60,
            3 * 60,
            4 * 60,
            2,
        )
    }

    pub const fn request_queue_delay(&self) -> DurationSeconds {
        self.request_queue_delay
    }
    pub const fn review_duration(&self) -> DurationSeconds {
        self.review_duration
    }
    pub const fn proposal_duration(&self) -> DurationSeconds {
        self.proposal_duration
    }
    pub const fn council_request_cooldown(&self) -> DurationSeconds {
        self.council_request_cooldown
    }
    pub const fn deferred_reconsideration_delay(&self) -> DurationSeconds {
        self.deferred_reconsideration_delay
    }
    pub const fn construction_mobilisation_delay(&self) -> DurationSeconds {
        self.construction_mobilisation_delay
    }
    pub const fn new_line_base_construction_duration(&self) -> DurationSeconds {
        self.new_line_base_construction_duration
    }
    pub const fn low_difficulty_seconds_per_kilometre(&self) -> u64 {
        self.low_difficulty_seconds_per_kilometre
    }
    pub const fn moderate_difficulty_seconds_per_kilometre(&self) -> u64 {
        self.moderate_difficulty_seconds_per_kilometre
    }
    pub const fn high_difficulty_seconds_per_kilometre(&self) -> u64 {
        self.high_difficulty_seconds_per_kilometre
    }
    pub const fn max_active_expansion_projects(&self) -> u32 {
        self.max_active_expansion_projects
    }
}

impl Default for AuthorityRules {
    fn default() -> Self {
        Self::provisional()
    }
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
    use super::*;

    #[test]
    fn provisional_authority_rules_define_the_slower_progression_target() {
        let rules = AuthorityRules::provisional();

        assert_eq!(rules.request_queue_delay().seconds(), 60 * 60);
        assert_eq!(rules.review_duration().seconds(), 2 * 60 * 60);
        assert_eq!(rules.proposal_duration().seconds(), 60 * 60);
        assert_eq!(rules.council_request_cooldown().seconds(), 24 * 60 * 60);
        assert_eq!(
            rules.deferred_reconsideration_delay().seconds(),
            24 * 60 * 60
        );
        assert_eq!(rules.construction_mobilisation_delay().seconds(), 60 * 60);
        assert_eq!(
            rules.new_line_base_construction_duration().seconds(),
            5 * 60 * 60
        );
        assert_eq!(rules.low_difficulty_seconds_per_kilometre(), 2 * 60);
        assert_eq!(rules.moderate_difficulty_seconds_per_kilometre(), 3 * 60);
        assert_eq!(rules.high_difficulty_seconds_per_kilometre(), 4 * 60);
        assert_eq!(rules.max_active_expansion_projects(), 2);
    }
}

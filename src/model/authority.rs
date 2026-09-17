//! Rail Authority, public infrastructure, and Rail Network domain types.

use serde::{Deserialize, Serialize};

use super::{
    CalculationError, ConstructionDifficulty, DistanceMetres, Electrification,
    InfrastructureProjectId, Money, MoneyPerKilometre, RailLineId, RailStationId, SettlementId,
    SpeedKilometresPerHour, TrackCount, UtcSeconds,
};

/// Current lifecycle stage of a Rail Authority infrastructure project.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InfrastructureProjectStatus {
    Requested,
    UnderReview,
    Proposed,
    Approved,
    Deferred,
    Rejected,
    Funding,
    Scheduled,
    Construction,
    Open,
    Cancelled,
}

/// Lifecycle timestamps recorded as an infrastructure project advances.
///
/// Only `requested_at` is required when a project is created. Later batches
/// own the transition rules that populate the optional milestones.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InfrastructureProjectTimeline {
    pub requested_at: UtcSeconds,
    pub review_started_at: Option<UtcSeconds>,
    pub proposed_at: Option<UtcSeconds>,
    pub approved_at: Option<UtcSeconds>,
    pub funding_completed_at: Option<UtcSeconds>,
    pub scheduled_start_at: Option<UtcSeconds>,
    pub construction_started_at: Option<UtcSeconds>,
    pub planned_completion_at: Option<UtcSeconds>,
    pub completed_at: Option<UtcSeconds>,
    pub deferred_at: Option<UtcSeconds>,
    pub cancelled_at: Option<UtcSeconds>,
    /// Number of times a Deferred request has been reopened for reconsideration.
    ///
    /// Persisting this keeps council pressure monotonic across save/load and lets
    /// later reconsiderations require a stronger rail-adoption signal.
    #[serde(default)]
    pub reconsideration_count: u8,
}

/// Financial commitment state for one public infrastructure project.
///
/// Player/operator contributions are introduced later. For now the Authority
/// can reserve part or all of the estimated cost from its investment budget.
pub const PROVISIONAL_OPERATOR_CONTRIBUTION_CAP_PERCENT: u64 = 20;
pub const PROVISIONAL_OPERATOR_CONTRIBUTION_TRANCHE_PERCENT: u64 = 10;
/// Provisional access-fee credit granted when an operator-funded project opens.
/// 115% gives the contribution a modest commercial return without creating ownership.
pub const PROVISIONAL_OPERATOR_ACCESS_CREDIT_PERCENT: u64 = 115;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct InfrastructureProjectFunding {
    pub estimated_cost: Money,
    pub authority_committed: Money,
    #[serde(default)]
    pub operator_contributed: Money,
    #[serde(default)]
    pub access_fee_credit_awarded: Money,
    #[serde(default)]
    pub access_fee_credit_remaining: Money,
}

impl InfrastructureProjectFunding {
    pub fn total_funded(&self) -> Result<Money, CalculationError> {
        self.authority_committed
            .checked_add(self.operator_contributed)
    }

    pub fn funding_gap(&self) -> Result<Money, CalculationError> {
        self.estimated_cost.checked_sub(self.total_funded()?)
    }

    pub fn operator_contribution_cap(&self) -> Result<Money, CalculationError> {
        let cents = i128::from(self.estimated_cost.cents())
            .checked_mul(i128::from(PROVISIONAL_OPERATOR_CONTRIBUTION_CAP_PERCENT))
            .ok_or(CalculationError::Overflow {
                operation: "operator infrastructure contribution cap",
            })?
            / 100;
        let cents = i64::try_from(cents).map_err(|_| CalculationError::Overflow {
            operation: "operator infrastructure contribution cap",
        })?;
        Ok(Money::from_cents(cents))
    }

    pub fn remaining_operator_contribution_capacity(&self) -> Result<Money, CalculationError> {
        self.operator_contribution_cap()?
            .checked_sub(self.operator_contributed)
    }

    pub fn suggested_operator_contribution(
        &self,
        company_funds: Money,
    ) -> Result<Money, CalculationError> {
        if company_funds <= Money::ZERO {
            return Ok(Money::ZERO);
        }
        let cents = i128::from(self.estimated_cost.cents())
            .checked_mul(i128::from(
                PROVISIONAL_OPERATOR_CONTRIBUTION_TRANCHE_PERCENT,
            ))
            .ok_or(CalculationError::Overflow {
                operation: "operator infrastructure contribution tranche",
            })?
            / 100;
        let cents = i64::try_from(cents).map_err(|_| CalculationError::Overflow {
            operation: "operator infrastructure contribution tranche",
        })?;
        let tranche = Money::from_cents(cents.max(1));
        Ok(tranche
            .min(self.funding_gap()?)
            .min(self.remaining_operator_contribution_capacity()?)
            .min(company_funds))
    }

    pub fn operator_access_credit_value(&self) -> Result<Money, CalculationError> {
        let cents = i128::from(self.operator_contributed.cents())
            .checked_mul(i128::from(PROVISIONAL_OPERATOR_ACCESS_CREDIT_PERCENT))
            .ok_or(CalculationError::Overflow {
                operation: "operator infrastructure access credit",
            })?
            / 100;
        let cents = i64::try_from(cents).map_err(|_| CalculationError::Overflow {
            operation: "operator infrastructure access credit",
        })?;
        Ok(Money::from_cents(cents))
    }

    pub fn award_operator_access_credit(&mut self) -> Result<Money, CalculationError> {
        if self.access_fee_credit_awarded > Money::ZERO {
            return Ok(self.access_fee_credit_awarded);
        }
        let credit = self.operator_access_credit_value()?;
        self.access_fee_credit_awarded = credit;
        self.access_fee_credit_remaining = credit;
        Ok(credit)
    }

    pub fn is_fully_funded(&self) -> bool {
        self.total_funded()
            .is_ok_and(|funded| funded >= self.estimated_cost)
    }
}

/// One Rail Station reserved by a planned New Line project.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedRailStation {
    pub id: RailStationId,
    pub settlement_id: SettlementId,
}

/// One physical Rail Line segment reserved by a planned New Line project.
///
/// Endpoints may reference either existing Rail Stations or Stations reserved
/// in the same project. Carrying the complete capabilities here lets opening a
/// project later materialize exactly the infrastructure that was approved.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedRailLine {
    pub id: RailLineId,
    pub first_station_id: RailStationId,
    pub second_station_id: RailStationId,
    pub distance: DistanceMetres,
    pub speed_limit: SpeedKilometresPerHour,
    pub track_count: TrackCount,
    pub electrification: Electrification,
    pub construction_difficulty: ConstructionDifficulty,
}

/// Physical scope and intended outcome of a Rail Authority project.
///
/// Only `NewLine` receives simulation behaviour in the first Authority phase;
/// the other variants are modeled now so later modernization work reuses the
/// same project and persistence framework.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InfrastructureProjectKind {
    NewLine {
        planned_stations: Vec<PlannedRailStation>,
        planned_lines: Vec<PlannedRailLine>,
    },
    SpeedUpgrade {
        rail_line_ids: Vec<RailLineId>,
        target_speed_limit: SpeedKilometresPerHour,
    },
    DoubleTracking {
        rail_line_ids: Vec<RailLineId>,
        target_track_count: TrackCount,
    },
    Electrification {
        rail_line_ids: Vec<RailLineId>,
    },
    Renewal {
        rail_line_ids: Vec<RailLineId>,
    },
    StationUpgrade {
        rail_station_ids: Vec<RailStationId>,
    },
}

/// One public infrastructure proposal tracked by the Rail Authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InfrastructureProject {
    pub id: InfrastructureProjectId,
    pub kind: InfrastructureProjectKind,
    pub status: InfrastructureProjectStatus,
    pub timeline: InfrastructureProjectTimeline,
    #[serde(default)]
    pub funding: InfrastructureProjectFunding,
}

impl InfrastructureProjectStatus {
    /// Whether this project currently occupies construction capacity on its scope.
    pub const fn is_under_construction(self) -> bool {
        matches!(self, Self::Construction)
    }

    /// Whether this project has reserved one of the Authority's construction slots.
    ///
    /// Scheduled work reserves capacity before crews mobilise so another project
    /// cannot be promised the same slot in the meantime.
    pub const fn reserves_construction_capacity(self) -> bool {
        matches!(self, Self::Scheduled | Self::Construction)
    }
}

impl InfrastructureProjectKind {
    /// Whether two projects compete for the same physical infrastructure.
    ///
    /// This deliberately models only conflicts RailQ can reason about today:
    /// shared existing Rail Lines, shared Station upgrades, and a Station
    /// upgrade on an endpoint where a New Line is being attached. More
    /// detailed work-site and possession conflicts belong to later simulation
    /// layers.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        if let (Some(left), Some(right)) = (self.rail_line_targets(), other.rail_line_targets()) {
            if ids_overlap(left, right) {
                return true;
            }
        }

        if let (Some(left), Some(right)) =
            (self.rail_station_targets(), other.rail_station_targets())
        {
            if ids_overlap(left, right) {
                return true;
            }
        }

        match (self, other) {
            (Self::NewLine { planned_lines, .. }, Self::StationUpgrade { rail_station_ids })
            | (Self::StationUpgrade { rail_station_ids }, Self::NewLine { planned_lines, .. }) => {
                planned_lines.iter().any(|line| {
                    rail_station_ids.contains(&line.first_station_id)
                        || rail_station_ids.contains(&line.second_station_id)
                })
            }
            _ => false,
        }
    }

    fn rail_line_targets(&self) -> Option<&[RailLineId]> {
        match self {
            Self::SpeedUpgrade { rail_line_ids, .. }
            | Self::DoubleTracking { rail_line_ids, .. }
            | Self::Electrification { rail_line_ids }
            | Self::Renewal { rail_line_ids } => Some(rail_line_ids),
            Self::NewLine { .. } | Self::StationUpgrade { .. } => None,
        }
    }

    fn rail_station_targets(&self) -> Option<&[RailStationId]> {
        match self {
            Self::StationUpgrade { rail_station_ids } => Some(rail_station_ids),
            _ => None,
        }
    }
}

impl InfrastructureProject {
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.kind.conflicts_with(&other.kind)
    }

    /// Whether an access-fee credit earned by this project applies to one Rail Line.
    pub fn access_credit_covers_line(&self, rail_line_id: RailLineId) -> bool {
        match &self.kind {
            InfrastructureProjectKind::NewLine { planned_lines, .. } => {
                planned_lines.iter().any(|line| line.id == rail_line_id)
            }
            InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
            | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
            | InfrastructureProjectKind::Electrification { rail_line_ids }
            | InfrastructureProjectKind::Renewal { rail_line_ids } => {
                rail_line_ids.contains(&rail_line_id)
            }
            InfrastructureProjectKind::StationUpgrade { .. } => false,
        }
    }
}

impl RailAuthority {
    /// Finds an active construction project that prevents `candidate` from
    /// starting work on the same infrastructure.
    pub fn blocking_construction_project(
        &self,
        candidate: &InfrastructureProject,
    ) -> Option<&InfrastructureProject> {
        self.infrastructure_projects.iter().find(|project| {
            project.id != candidate.id
                && project.status.is_under_construction()
                && project.conflicts_with(candidate)
        })
    }

    /// Number of major infrastructure projects currently occupying Authority
    /// construction capacity.
    pub fn active_construction_count(&self) -> u32 {
        u32::try_from(
            self.infrastructure_projects
                .iter()
                .filter(|project| project.status.is_under_construction())
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Number of projects that have reserved a construction slot, including
    /// projects whose crews are scheduled but have not mobilised yet.
    pub fn reserved_construction_count(&self) -> u32 {
        u32::try_from(
            self.infrastructure_projects
                .iter()
                .filter(|project| project.status.reserves_construction_capacity())
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Finds scheduled or active construction that conflicts with `candidate`.
    pub fn blocking_reserved_project(
        &self,
        candidate: &InfrastructureProject,
    ) -> Option<&InfrastructureProject> {
        self.infrastructure_projects.iter().find(|project| {
            project.id != candidate.id
                && project.status.reserves_construction_capacity()
                && project.conflicts_with(candidate)
        })
    }

    /// Remaining major-project construction slots in the active works programme.
    pub fn construction_slots_remaining(&self) -> u32 {
        self.construction_capacity
            .saturating_sub(self.reserved_construction_count())
    }

    /// Maximum number of projects the Authority actively finances at once.
    ///
    /// Keeping a small funding pipeline prevents every approved proposal from
    /// becoming a permanently half-funded project when public cash is tight.
    /// The provisional rule keeps two funding projects per construction slot.
    pub fn funding_pipeline_capacity(&self) -> u32 {
        self.construction_capacity.max(1).saturating_mul(2)
    }

    /// Number of projects currently occupying the active funding pipeline.
    pub fn active_funding_count(&self) -> u32 {
        u32::try_from(
            self.infrastructure_projects
                .iter()
                .filter(|project| project.status == InfrastructureProjectStatus::Funding)
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Whether a fully funded project can reserve a construction slot.
    pub fn can_schedule_construction(&self, candidate: &InfrastructureProject) -> bool {
        self.construction_slots_remaining() > 0
            && self.blocking_reserved_project(candidate).is_none()
    }

    /// Whether an additional, not-yet-scheduled project could start construction
    /// right now. Scheduled projects already own their reserved slot and are
    /// handled by the construction-start lifecycle.
    pub fn can_start_construction(&self, candidate: &InfrastructureProject) -> bool {
        self.construction_slots_remaining() > 0
            && self.blocking_construction_project(candidate).is_none()
    }
}

fn ids_overlap<T: Eq>(left: &[T], right: &[T]) -> bool {
    left.iter().any(|id| right.contains(id))
}

/// The public funds controlled by a Rail Authority.
///
/// `treasury` is the total cash held by the Authority. Maintenance and
/// committed investment are earmarks inside that treasury; the remainder is
/// available for future infrastructure projects. `carried_over_funds` records
/// the portion brought forward from an earlier budget cycle once fiscal
/// periods are introduced.
pub const PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION: Money = Money::from_cents(10_000_000);
/// Number of seconds in one UTC calendar day.
const SECONDS_PER_UTC_DAY: i64 = 24 * 60 * 60;
/// Provisional maintenance reserve per physical track-kilometre and budget cycle.
///
/// This is deliberately a simple balancing value until RailQ models actual
/// infrastructure condition and renewal work.
pub const PROVISIONAL_MAINTENANCE_RESERVE_PER_TRACK_KILOMETRE: MoneyPerKilometre =
    MoneyPerKilometre(25_000);

/// Maximum number of major infrastructure projects the Authority can have
/// under construction at the same time in the initial simulation.
pub const PROVISIONAL_CONSTRUCTION_CAPACITY: u32 = 1;

const fn default_construction_capacity() -> u32 {
    PROVISIONAL_CONSTRUCTION_CAPACITY
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailAuthorityFinances {
    pub treasury: Money,
    pub maintenance_reserve: Money,
    pub committed_investment: Money,
    pub carried_over_funds: Money,
    #[serde(default = "default_regional_public_allocation")]
    pub regional_public_allocation: Money,
    #[serde(default)]
    pub infrastructure_access_fee_revenue: Money,
    #[serde(default)]
    pub next_fiscal_period_at: Option<UtcSeconds>,
}

const fn default_regional_public_allocation() -> Money {
    PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION
}

impl Default for RailAuthorityFinances {
    fn default() -> Self {
        Self {
            treasury: Money::ZERO,
            maintenance_reserve: Money::ZERO,
            committed_investment: Money::ZERO,
            carried_over_funds: Money::ZERO,
            regional_public_allocation: PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION,
            infrastructure_access_fee_revenue: Money::ZERO,
            next_fiscal_period_at: None,
        }
    }
}

/// Returns the first UTC midnight strictly after `timestamp`.
pub fn next_utc_midnight_after(timestamp: UtcSeconds) -> Result<UtcSeconds, CalculationError> {
    let day = timestamp.unix_seconds().div_euclid(SECONDS_PER_UTC_DAY);
    let next_day = day.checked_add(1).ok_or(CalculationError::Overflow {
        operation: "Authority fiscal day increment",
    })?;
    let next = next_day
        .checked_mul(SECONDS_PER_UTC_DAY)
        .ok_or(CalculationError::Overflow {
            operation: "Authority fiscal midnight calculation",
        })?;
    Ok(UtcSeconds::from_unix_seconds(next))
}

impl RailAuthorityFinances {
    /// Creates the initial Authority budget with the first recurring public
    /// allocation already deposited.
    pub fn with_initial_public_allocation() -> Self {
        Self {
            treasury: PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION,
            ..Self::default()
        }
    }

    /// Starts the Authority fiscal calendar at the next UTC midnight.
    pub fn initialize_fiscal_calendar(
        &mut self,
        started_at: UtcSeconds,
    ) -> Result<UtcSeconds, CalculationError> {
        let next = next_utc_midnight_after(started_at)?;
        self.next_fiscal_period_at = Some(next);
        Ok(next)
    }

    /// Deposits one regional public-allocation cycle into the Authority treasury.
    pub fn receive_regional_public_allocation(&mut self) -> Result<Money, CalculationError> {
        self.treasury = self.treasury.checked_add(self.regional_public_allocation)?;
        Ok(self.regional_public_allocation)
    }

    /// Records an infrastructure access fee paid by the passenger operator
    /// and deposits it into the Rail Authority treasury.
    pub fn receive_infrastructure_access_fee(
        &mut self,
        amount: Money,
    ) -> Result<(), CalculationError> {
        let treasury = self.treasury.checked_add(amount)?;
        let revenue = self.infrastructure_access_fee_revenue.checked_add(amount)?;
        self.treasury = treasury;
        self.infrastructure_access_fee_revenue = revenue;
        Ok(())
    }

    /// Reserves as much of the current maintenance requirement as the
    /// uncommitted treasury can support. Detailed degradation/renewal costs
    /// are intentionally deferred to the later infrastructure-condition phase.
    pub fn refresh_maintenance_reserve(
        &mut self,
        network: &RailNetwork,
    ) -> Result<Money, CalculationError> {
        let required = network.provisional_maintenance_reserve()?;
        let available_after_commitments = self.treasury.checked_sub(self.committed_investment)?;
        self.maintenance_reserve = required.min(available_after_commitments);
        Ok(self.maintenance_reserve)
    }

    /// Money that is neither reserved for maintenance nor committed to an
    /// approved infrastructure project.
    pub fn uncommitted_investment(&self) -> Result<Money, CalculationError> {
        self.treasury
            .checked_sub(self.maintenance_reserve)?
            .checked_sub(self.committed_investment)
    }
}

/// The public owner of a Region's Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailAuthority {
    pub name: String,
    pub rail_network: RailNetwork,
    #[serde(default)]
    pub finances: RailAuthorityFinances,
    #[serde(default = "default_construction_capacity")]
    pub construction_capacity: u32,
    #[serde(default)]
    pub infrastructure_projects: Vec<InfrastructureProject>,
}

/// The physical infrastructure owned by a Rail Authority.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailNetwork {
    pub rail_stations: Vec<RailStation>,
    pub rail_lines: Vec<RailLine>,
}

impl RailNetwork {
    /// Calculates the provisional reserve needed to maintain the current
    /// physical network. Double-track sections count twice because they
    /// contain twice as much running track to maintain.
    pub fn provisional_maintenance_reserve(&self) -> Result<Money, CalculationError> {
        self.rail_lines.iter().try_fold(Money::ZERO, |total, line| {
            let single_track = PROVISIONAL_MAINTENANCE_RESERVE_PER_TRACK_KILOMETRE
                .checked_charge(line.distance)?;
            let line_reserve = single_track.checked_mul(u64::from(line.track_count.tracks()))?;
            total.checked_add(line_reserve)
        })
    }
}

/// A facility providing one Settlement access to the Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailStation {
    pub id: RailStationId,
    pub settlement_id: SettlementId,
}

/// One stable physical Rail Line segment between two Rail Stations.
///
/// `RailLineId` already acts as the stable segment identity needed by future
/// Authority projects, so RailQ does not introduce a second parallel ID type.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailLine {
    pub id: RailLineId,
    pub first_station_id: RailStationId,
    pub second_station_id: RailStationId,
    pub distance: DistanceMetres,
    #[serde(default = "default_rail_line_speed_limit")]
    pub speed_limit: SpeedKilometresPerHour,
    #[serde(default)]
    pub track_count: TrackCount,
    #[serde(default)]
    pub electrification: Electrification,
    #[serde(default)]
    pub construction_difficulty: ConstructionDifficulty,
}

fn default_rail_line_speed_limit() -> SpeedKilometresPerHour {
    SpeedKilometresPerHour::new(70).expect("RailQ's default Rail Line speed limit is valid")
}

//! Core domain value types.
//!
//! All calculations use integer units. When a calculation has a fractional
//! result, RailQ rounds up: a partial cent or partial second is charged or
//! scheduled as one whole unit. This keeps quotes deterministic and never
//! understates a cost or Journey duration.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::balance::BalanceConfig;

mod ids;

pub use ids::{
    InfrastructureProjectId, JourneyId, RailLineId, RailStationId, ServiceId, SettlementId,
    TrainId, TrainModelId,
};

mod units;

pub use units::{
    CalculationError, ConstructionDifficulty, DistanceMetres, DurationSeconds, Electrification,
    Money, MoneyPerKilometre, PassengerCapacity, SpeedKilometresPerHour, SpeedMetresPerSecond,
    TrackCount, UtcSeconds, ValidationError,
};

mod vehicle;

pub use vehicle::{
    EuropeanVehicleNumber, EuropeanVehicleNumberError, TrainNickname, TrainNicknameError,
    VehicleKeeperMark, VehicleKeeperMarkError,
};

#[cfg(test)]
use vehicle::evn_check_digit;



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
    /// Stable fictional railway registration identity used for vehicle numbering.
    #[serde(default)]
    pub railway_registration: RailwayRegistration,
    /// The total Population of every Settlement in this Region.
    pub population: u64,
    pub settlements: Vec<Settlement>,
    /// Persistent significant regional railway developments shown in the Bulletin workspace.
    #[serde(default)]
    pub bulletin: Vec<BulletinEntry>,
    pub rail_authority: RailAuthority,
}

/// High-level source/type of one persistent regional Bulletin item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BulletinCategory {
    Local,
    Authority,
    Construction,
    Network,
}

/// One persistent, player-facing record of a significant railway-world event.
///
/// Routine Train movements deliberately do not belong here; the Bulletin is a
/// compact history of developments that materially change or explain the world.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BulletinEntry {
    pub occurred_at: UtcSeconds,
    pub category: BulletinCategory,
    pub headline: String,
    pub detail: String,
}

/// Stable fictional registration identity assigned when a Region is generated.
///
/// `numeric_code` is deliberately two digits so it can later occupy the
/// country-code position of RailQ's EVN-style vehicle numbers. `mark` is the
/// short alphabetic Region marking shown alongside that identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailwayRegistration {
    pub numeric_code: u8,
    pub mark: String,
}

impl Default for RailwayRegistration {
    fn default() -> Self {
        Self {
            numeric_code: 99,
            mark: "RQ".into(),
        }
    }
}

impl RailwayRegistration {
    pub fn display_code(&self) -> String {
        format!("{:02}", self.numeric_code)
    }
}

/// Stable geographical position inside the generated Region.
///
/// Coordinates are world-space kilometres, not terminal cells. The UI may
/// scale them to any terminal size while simulation systems can derive
/// consistent physical distances from the same geography.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorldPosition {
    pub x: i32,
    pub y: i32,
}

impl WorldPosition {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A populated place in a Region, with or without railway access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settlement {
    pub id: SettlementId,
    pub name: String,
    pub population: u64,
    #[serde(default)]
    pub position: WorldPosition,
}

/// Current lifecycle stage of a Rail Authority infrastructure project.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InfrastructureProjectStatus {
    Requested,
    UnderReview,
    Proposed,
    Approved,
    Deferred,
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
        self.authority_committed.checked_add(self.operator_contributed)
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

    pub fn suggested_operator_contribution(&self, company_funds: Money) -> Result<Money, CalculationError> {
        if company_funds <= Money::ZERO {
            return Ok(Money::ZERO);
        }
        let cents = i128::from(self.estimated_cost.cents())
            .checked_mul(i128::from(PROVISIONAL_OPERATOR_CONTRIBUTION_TRANCHE_PERCENT))
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
        self.total_funded().is_ok_and(|funded| funded >= self.estimated_cost)
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
pub fn next_utc_midnight_after(
    timestamp: UtcSeconds,
) -> Result<UtcSeconds, CalculationError> {
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



/// The passenger railway company controlled by the player.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlayerCompany {
    pub name: String,
    pub vehicle_keeper_mark: VehicleKeeperMark,
    pub funds: Money,
    pub fleet: Fleet,
    pub passenger_services: Vec<PassengerService>,
}

/// All Trains owned by the Player Company.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Fleet {
    pub trains: Vec<Train>,
    /// Next compact display number for an owned Train. This is not the Train
    /// identity; persisted Train IDs are UUID v4 values.
    #[serde(default = "default_next_train_display_number")]
    pub next_train_display_number: u64,
    /// Next EVN unit number to allocate for each persistent Train model.
    ///
    /// Values are one-based; 1000 means the model's 001–999 allocation is
    /// exhausted. Keeping the counter after resale prevents EVN reuse.
    #[serde(default)]
    pub next_evn_unit_by_model: BTreeMap<TrainModelId, u16>,
}

const fn default_next_train_display_number() -> u64 {
    1
}

impl Default for Fleet {
    fn default() -> Self {
        Self {
            trains: Vec::new(),
            next_train_display_number: default_next_train_display_number(),
            next_evn_unit_by_model: BTreeMap::new(),
        }
    }
}

/// Passenger rolling stock owned by the Player Company.
///
/// Immutable technical specifications live in the central Train catalogue. An
/// owned Train persists only the stable catalogue model ID plus instance state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Train {
    pub id: TrainId,
    /// Permanent official vehicle number assigned when the Train joins the Fleet.
    pub evn: EuropeanVehicleNumber,
    /// Optional player-facing name. It never changes the official EVN.
    #[serde(default)]
    pub nickname: Option<TrainNickname>,
    pub status: TrainStatus,
    pub model_id: TrainModelId,
    /// The amount actually paid when this Train joined the Fleet.
    ///
    /// Resale is calculated from this historical purchase price, not the
    /// current catalogue price.
    pub original_purchase_price: Money,
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

#[cfg(test)]
mod tests {
    use std::{any::TypeId, collections::HashSet};

    use super::*;

    #[test]
    fn infrastructure_projects_conflict_when_they_target_the_same_rail_line() {
        let speed_upgrade = InfrastructureProjectKind::SpeedUpgrade {
            rail_line_ids: vec![RailLineId::new(1)],
            target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
        };
        let electrification = InfrastructureProjectKind::Electrification {
            rail_line_ids: vec![RailLineId::new(1)],
        };
        let unrelated_renewal = InfrastructureProjectKind::Renewal {
            rail_line_ids: vec![RailLineId::new(2)],
        };

        assert!(speed_upgrade.conflicts_with(&electrification));
        assert!(electrification.conflicts_with(&speed_upgrade));
        assert!(!speed_upgrade.conflicts_with(&unrelated_renewal));
    }

    #[test]
    fn station_upgrade_conflicts_with_new_line_work_at_the_same_station() {
        let new_line = InfrastructureProjectKind::NewLine {
            planned_stations: vec![PlannedRailStation {
                id: RailStationId::new(5),
                settlement_id: SettlementId::new(5),
            }],
            planned_lines: vec![PlannedRailLine {
                id: RailLineId::new(4),
                first_station_id: RailStationId::new(2),
                second_station_id: RailStationId::new(5),
                distance: DistanceMetres::new(12_000).unwrap(),
                speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                track_count: TrackCount::SINGLE,
                electrification: Electrification::None,
                construction_difficulty: ConstructionDifficulty::Moderate,
            }],
        };
        let same_station = InfrastructureProjectKind::StationUpgrade {
            rail_station_ids: vec![RailStationId::new(2)],
        };
        let different_station = InfrastructureProjectKind::StationUpgrade {
            rail_station_ids: vec![RailStationId::new(3)],
        };

        assert!(new_line.conflicts_with(&same_station));
        assert!(same_station.conflicts_with(&new_line));
        assert!(!new_line.conflicts_with(&different_station));
    }

    #[test]
    fn rail_authority_finances_expose_uncommitted_investment() {
        let finances = RailAuthorityFinances {
            treasury: Money::from_cents(1_000_000),
            maintenance_reserve: Money::from_cents(200_000),
            committed_investment: Money::from_cents(350_000),
            carried_over_funds: Money::from_cents(100_000),
            regional_public_allocation: Money::from_cents(500_000),
            infrastructure_access_fee_revenue: Money::ZERO,
            next_fiscal_period_at: None,
        };

        assert_eq!(
            finances.uncommitted_investment().unwrap(),
            Money::from_cents(450_000)
        );
    }

    #[test]
    fn regional_public_allocation_is_recurring_revenue() {
        let mut finances = RailAuthorityFinances::default();
        assert_eq!(finances.treasury, Money::ZERO);

        let received = finances.receive_regional_public_allocation().unwrap();
        assert_eq!(received, PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION);
        assert_eq!(finances.treasury, PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION);

        finances.receive_regional_public_allocation().unwrap();
        assert_eq!(
            finances.treasury,
            PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION
                .checked_mul(2)
                .unwrap()
        );
    }

    #[test]
    fn infrastructure_access_fees_are_authority_revenue() {
        let mut finances = RailAuthorityFinances::with_initial_public_allocation();
        let before = finances.treasury;
        let fee = Money::from_cents(42_500);

        finances.receive_infrastructure_access_fee(fee).unwrap();

        assert_eq!(finances.treasury, before.checked_add(fee).unwrap());
        assert_eq!(finances.infrastructure_access_fee_revenue, fee);
    }

    #[test]
    fn maintenance_reserve_scales_with_track_kilometres() {
        let network = RailNetwork {
            rail_stations: vec![],
            rail_lines: vec![
                RailLine {
                    id: RailLineId::new(1),
                    first_station_id: RailStationId::new(1),
                    second_station_id: RailStationId::new(2),
                    distance: DistanceMetres::new(10_000).unwrap(),
                    speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                    track_count: TrackCount::SINGLE,
                    electrification: Electrification::None,
                    construction_difficulty: ConstructionDifficulty::Moderate,
                },
                RailLine {
                    id: RailLineId::new(2),
                    first_station_id: RailStationId::new(2),
                    second_station_id: RailStationId::new(3),
                    distance: DistanceMetres::new(5_000).unwrap(),
                    speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                    track_count: TrackCount::DOUBLE,
                    electrification: Electrification::None,
                    construction_difficulty: ConstructionDifficulty::Moderate,
                },
            ],
        };

        assert_eq!(
            network.provisional_maintenance_reserve().unwrap(),
            Money::from_cents(500_000)
        );
    }

    #[test]
    fn maintenance_reserve_never_overcommits_the_treasury() {
        let network = RailNetwork {
            rail_stations: vec![],
            rail_lines: vec![RailLine {
                id: RailLineId::new(1),
                first_station_id: RailStationId::new(1),
                second_station_id: RailStationId::new(2),
                distance: DistanceMetres::new(100_000).unwrap(),
                speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                track_count: TrackCount::SINGLE,
                electrification: Electrification::None,
                construction_difficulty: ConstructionDifficulty::Moderate,
            }],
        };
        let mut finances = RailAuthorityFinances {
            treasury: Money::from_cents(1_000_000),
            maintenance_reserve: Money::ZERO,
            committed_investment: Money::from_cents(250_000),
            carried_over_funds: Money::ZERO,
            regional_public_allocation: PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION,
            infrastructure_access_fee_revenue: Money::ZERO,
            next_fiscal_period_at: None,
        };

        assert_eq!(
            finances.refresh_maintenance_reserve(&network).unwrap(),
            Money::from_cents(750_000)
        );
    }

    #[test]
    fn rail_authority_only_reports_active_construction_as_a_blocker() {
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(1),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        let blocker = InfrastructureProject {
            id: InfrastructureProjectId::new(1),
            kind: InfrastructureProjectKind::SpeedUpgrade {
                rail_line_ids: vec![RailLineId::new(1)],
                target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        };
        let approved = InfrastructureProject {
            id: InfrastructureProjectId::new(2),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Approved,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        };
        let candidate = InfrastructureProject {
            id: InfrastructureProjectId::new(3),
            kind: InfrastructureProjectKind::Electrification {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Scheduled,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        };
        let authority = RailAuthority {
            name: "Test Authority".into(),
            rail_network: RailNetwork::default(),
            finances: RailAuthorityFinances::default(),
            construction_capacity: PROVISIONAL_CONSTRUCTION_CAPACITY,
            infrastructure_projects: vec![approved, blocker.clone()],
        };

        assert_eq!(
            authority.blocking_construction_project(&candidate),
            Some(&blocker)
        );
    }

    #[test]
    fn construction_capacity_blocks_unrelated_projects_when_all_slots_are_used() {
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(1),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        let active = InfrastructureProject {
            id: InfrastructureProjectId::new(10),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        };
        let unrelated = InfrastructureProject {
            id: InfrastructureProjectId::new(11),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(2)],
            },
            status: InfrastructureProjectStatus::Scheduled,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        };
        let authority = RailAuthority {
            name: "Test Authority".into(),
            rail_network: RailNetwork::default(),
            finances: RailAuthorityFinances::default(),
            construction_capacity: 1,
            infrastructure_projects: vec![active],
        };

        assert_eq!(authority.active_construction_count(), 1);
        assert_eq!(authority.construction_slots_remaining(), 0);
        assert!(!authority.can_start_construction(&unrelated));
    }

    #[test]
    fn spare_capacity_allows_non_conflicting_construction() {
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(1),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        let active = InfrastructureProject {
            id: InfrastructureProjectId::new(20),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        };
        let unrelated = InfrastructureProject {
            id: InfrastructureProjectId::new(21),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(2)],
            },
            status: InfrastructureProjectStatus::Scheduled,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        };
        let authority = RailAuthority {
            name: "Test Authority".into(),
            rail_network: RailNetwork::default(),
            finances: RailAuthorityFinances::default(),
            construction_capacity: 2,
            infrastructure_projects: vec![active],
        };

        assert_eq!(authority.construction_slots_remaining(), 1);
        assert!(authority.can_start_construction(&unrelated));
    }

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
    fn evn_check_digit_matches_the_standard_modulo_ten_example() {
        assert_eq!(evn_check_digit("31513320198"), Some(0));
    }

    #[test]
    fn generates_and_formats_a_dmu_vehicle_number() {
        let evn = EuropeanVehicleNumber::generate(95, 72, 70, 1).unwrap();
        assert_eq!(evn.as_str(), "957200700012");
        assert_eq!(evn.formatted(), "95 72 0070 001-2");
        assert_eq!(evn.vehicle_type_code(), 95);
        assert_eq!(evn.registration_code(), 72);
        assert_eq!(evn.series_code(), 70);
        assert_eq!(evn.unit_number(), 1);
        assert_eq!(EuropeanVehicleNumber::parse(evn.as_str()).unwrap(), evn);
    }

    #[test]
    fn train_nickname_trims_and_preserves_player_casing() {
        let nickname = TrainNickname::parse("  Little Runner  ").unwrap();
        assert_eq!(nickname.as_str(), "Little Runner");
    }

    #[test]
    fn train_nickname_rejects_empty_and_overlong_values() {
        assert_eq!(TrainNickname::parse("   "), Err(TrainNicknameError::Empty));
        assert_eq!(
            TrainNickname::parse(&"x".repeat(TrainNickname::MAX_CHARACTERS + 1)),
            Err(TrainNicknameError::TooLong)
        );
    }

    #[test]
    fn rejects_an_invalid_evn_check_digit() {
        assert_eq!(
            EuropeanVehicleNumber::parse("957200700013"),
            Err(EuropeanVehicleNumberError::InvalidCheckDigit)
        );
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
                SpeedKilometresPerHour::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                TrackCount::new(value),
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

        let fast_train = SpeedMetresPerSecond::new(33).unwrap();
        assert_eq!(
            DistanceMetres::new(1_000)
                .unwrap()
                .journey_duration_with_speed_limit(
                    fast_train,
                    SpeedKilometresPerHour::new(70).unwrap(),
                )
                .unwrap(),
            DurationSeconds::from_seconds(52)
        );
        assert_eq!(
            DistanceMetres::new(1_000)
                .unwrap()
                .journey_duration_with_speed_limit(
                    fast_train,
                    SpeedKilometresPerHour::new(160).unwrap(),
                )
                .unwrap(),
            DurationSeconds::from_seconds(31)
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
    fn authority_fiscal_calendar_uses_the_next_utc_midnight() {
        assert_eq!(
            next_utc_midnight_after(UtcSeconds::from_unix_seconds(0)).unwrap(),
            UtcSeconds::from_unix_seconds(86_400)
        );
        assert_eq!(
            next_utc_midnight_after(UtcSeconds::from_unix_seconds(86_399)).unwrap(),
            UtcSeconds::from_unix_seconds(86_400)
        );
        assert_eq!(
            next_utc_midnight_after(UtcSeconds::from_unix_seconds(86_400)).unwrap(),
            UtcSeconds::from_unix_seconds(172_800)
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
    fn vehicle_keeper_mark_generates_from_company_name_and_validates_edits() {
        assert_eq!(
            VehicleKeeperMark::generated_from_company_name("One More Prime").as_str(),
            "OMP"
        );
        assert_eq!(
            VehicleKeeperMark::generated_from_company_name("Northstar").as_str(),
            "NORTH"
        );
        assert_eq!(VehicleKeeperMark::parse(" omp ").unwrap().as_str(), "OMP");
        assert_eq!(
            VehicleKeeperMark::parse("O"),
            Err(VehicleKeeperMarkError::InvalidLength)
        );
        assert_eq!(
            VehicleKeeperMark::parse("O1P"),
            Err(VehicleKeeperMarkError::InvalidCharacter)
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
                railway_registration: RailwayRegistration {
                    numeric_code: 67,
                    mark: "VA".into(),
                },
                population: 1_000,
                settlements: vec![Settlement {
                    id: settlement_id,
                    name: "Alden".into(),
                    population: 1_000,
                    position: crate::model::WorldPosition::default(),
                }],
                bulletin: vec![],
                rail_authority: RailAuthority {
                    name: "Varelia Rail Authority".into(),
                    rail_network: RailNetwork {
                        rail_stations: vec![RailStation {
                            id: station_id,
                            settlement_id,
                        }],
                        rail_lines: vec![],
                    },
                    finances: RailAuthorityFinances::default(),
                    construction_capacity: PROVISIONAL_CONSTRUCTION_CAPACITY,
                    infrastructure_projects: vec![],
                },
            },
            player_company: PlayerCompany {
                name: "Alden Passenger".into(),
                vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name(
                    "Alden Passenger",
                ),
                funds: Money::from_cents(10_000),
                fleet: Fleet {
                    next_train_display_number: 2,
                    trains: vec![Train {
                        id: train_id,
                        evn: EuropeanVehicleNumber::generate(95, 67, 701, 1).unwrap(),
                        nickname: None,
                        status: TrainStatus::Ready { at: station_id },
                        model_id: TrainModelId::new("helvetra-r70"),
                        original_purchase_price: Money::from_cents(5_000),
                    }],
                    next_evn_unit_by_model: [(TrainModelId::new("helvetra-r70"), 2)]
                        .into_iter()
                        .collect(),
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
                balance: BalanceConfig::new(rate, rate, Money::from_cents(10_000)),
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
            finances: RailAuthorityFinances::default(),
            construction_capacity: PROVISIONAL_CONSTRUCTION_CAPACITY,
            infrastructure_projects: vec![],
        };
        let company = PlayerCompany {
            name: "Alden Passenger".into(),
            vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name("Alden Passenger"),
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

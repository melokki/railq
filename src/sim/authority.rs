//! Rail Authority infrastructure planning helpers.
//!
//! Candidate evaluation and the early Rail Authority planning lifecycle.
//!
//! This layer currently advances projects through public funding, scheduling,
//! and fixed-duration construction. Opening is introduced later.

use std::{collections::BTreeSet, error::Error, fmt};

use crate::model::{
    AuthorityRules, BulletinCategory, BulletinEntry, CalculationError, ConstructionDifficulty,
    DistanceMetres, DurationSeconds, Electrification, GameState, InfrastructureProject,
    InfrastructureProjectFunding, InfrastructureProjectId, InfrastructureProjectKind,
    InfrastructureProjectStatus, InfrastructureProjectTimeline, Money, MoneyPerKilometre,
    OriginDestinationDemand, PlannedRailLine, PlannedRailStation, RailLine, RailLineId,
    RailStation, RailStationId, Region, SettlementId, SpeedKilometresPerHour, TrackCount,
    UtcSeconds,
};

/// One provisional new-line opportunity evaluated by the Rail Authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionCandidate {
    pub settlement_id: SettlementId,
    pub connection_station_id: RailStationId,
    pub estimated_distance: DistanceMetres,
    pub construction_difficulty: ConstructionDifficulty,
    pub estimated_cost: Money,
    pub score: ConnectionCandidateScore,
}

/// Explainable components of the Authority's provisional candidate ranking.
///
/// Positive components reward demand and network/public value. Construction
/// cost is represented separately as a penalty so a high-value but expensive
/// regional project can still outrank a cheap low-value one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionCandidateScore {
    pub population: u32,
    pub latent_demand: u32,
    pub network_usefulness: u32,
    pub regional_development: u32,
    pub construction_cost_penalty: u32,
    pub total: i32,
}

// A local council considers a rail connection politically justified only after
// a nearby existing corridor has become meaningfully established. Market
// maturity can rise only through completed passenger trips, so this threshold
// reacts to real railway use rather than Train ownership or elapsed time.
pub(crate) const COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS: u16 = 4_000;
// Councils do not submit connection requests back-to-back. The persisted
// Authority rules own this cooldown so progression pacing stays stable per save.
// A Deferred request can be reopened later, but only after a meaningful delay
// and stronger evidence that the surrounding railway is succeeding. Each
// reconsideration requires another 15 percentage points of local maturity.
const DEFERRED_RECONSIDERATION_BASE_MATURITY_BASIS_POINTS: u16 = 5_500;
const DEFERRED_RECONSIDERATION_STEP_BASIS_POINTS: u16 = 1_500;
// Provisional minimum public-value score required for a council request to
// survive Authority review. The score already balances population, latent
// demand, network usefulness, regional-development value, and construction
// cost (which incorporates distance and difficulty).
pub(crate) const AUTHORITY_APPROVAL_SCORE_THRESHOLD: i32 = 250;
// Construction mobilisation and physical-work cadence are persisted in
// `AuthorityRules`, keeping progression deterministic for each save.
/// Maximum number of missed daily fiscal periods applied when RailQ catches up
/// after being closed. Older missed periods are skipped so long absences do not
/// turn into unlimited unattended public funding.
const MAX_OFFLINE_FISCAL_CATCHUP_PERIODS: u32 = 3;

fn project_target_settlement_name(region: &Region, project_index: usize) -> String {
    let Some(project) = region
        .rail_authority
        .infrastructure_projects
        .get(project_index)
    else {
        return "Regional".into();
    };
    let InfrastructureProjectKind::NewLine {
        planned_stations, ..
    } = &project.kind
    else {
        return "Regional".into();
    };
    let Some(planned_station) = planned_stations.first() else {
        return "Regional".into();
    };
    region
        .settlements
        .iter()
        .find(|settlement| settlement.id == planned_station.settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| "Regional".into())
}

fn push_bulletin(
    region: &mut Region,
    occurred_at: UtcSeconds,
    category: BulletinCategory,
    headline: String,
    detail: String,
) {
    region.bulletin.push(BulletinEntry {
        occurred_at,
        category,
        headline,
        detail,
    });
}

/// Why an explicit Rail Authority project action cannot be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InfrastructureProjectActionError {
    ProjectNotFound {
        project_id: InfrastructureProjectId,
    },
    CannotCancel {
        project_id: InfrastructureProjectId,
        status: InfrastructureProjectStatus,
    },
    CannotContribute {
        project_id: InfrastructureProjectId,
        status: InfrastructureProjectStatus,
    },
    InvalidContribution,
    ContributionExceedsFundingGap,
    ContributionExceedsOperatorCap,
    InsufficientCompanyFunds,
    Calculation(CalculationError),
}

impl fmt::Display for InfrastructureProjectActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectNotFound { project_id } => {
                write!(
                    formatter,
                    "Infrastructure Project {} was not found",
                    project_id.uuid()
                )
            }
            Self::CannotCancel { project_id, status } => write!(
                formatter,
                "Infrastructure Project {} cannot be cancelled while {status:?}",
                project_id.uuid()
            ),
            Self::CannotContribute { project_id, status } => write!(
                formatter,
                "Infrastructure Project {} cannot accept an operator contribution while {status:?}",
                project_id.uuid()
            ),
            Self::InvalidContribution => {
                write!(formatter, "Infrastructure contribution must be positive")
            }
            Self::ContributionExceedsFundingGap => {
                write!(
                    formatter,
                    "Infrastructure contribution exceeds the remaining funding gap"
                )
            }
            Self::ContributionExceedsOperatorCap => {
                write!(
                    formatter,
                    "Infrastructure contribution exceeds the operator contribution cap"
                )
            }
            Self::InsufficientCompanyFunds => {
                write!(
                    formatter,
                    "Company Funds are too low for this infrastructure contribution"
                )
            }
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for InfrastructureProjectActionError {}

impl From<CalculationError> for InfrastructureProjectActionError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Cancels a project before physical construction starts and releases any
/// Authority investment commitment back to the uncommitted budget.
///
/// Construction and completed projects are intentionally terminal here. Future
/// operator contributions will reuse this path to refund their earmarked funds.
pub fn cancel_infrastructure_project(
    region: &mut Region,
    project_id: InfrastructureProjectId,
    now: UtcSeconds,
) -> Result<(), InfrastructureProjectActionError> {
    let index = region
        .rail_authority
        .infrastructure_projects
        .iter()
        .position(|project| project.id == project_id)
        .ok_or(InfrastructureProjectActionError::ProjectNotFound { project_id })?;
    let target_name = project_target_settlement_name(region, index);
    let authority = &mut region.rail_authority;

    let status = authority.infrastructure_projects[index].status;
    if matches!(
        status,
        InfrastructureProjectStatus::Construction
            | InfrastructureProjectStatus::Open
            | InfrastructureProjectStatus::Cancelled
    ) {
        return Err(InfrastructureProjectActionError::CannotCancel { project_id, status });
    }

    let authority_committed = authority.infrastructure_projects[index]
        .funding
        .authority_committed;
    authority.finances.committed_investment = authority
        .finances
        .committed_investment
        .checked_sub(authority_committed)?;

    let project = &mut authority.infrastructure_projects[index];
    project.funding.authority_committed = Money::ZERO;
    project.status = InfrastructureProjectStatus::Cancelled;
    project.timeline.cancelled_at = Some(now);

    push_bulletin(
        region,
        now,
        BulletinCategory::Authority,
        format!("{target_name} connection cancelled"),
        "The Rail Authority cancelled the project before physical construction began.".into(),
    );

    Ok(())
}

/// Transfers Player Company cash into one approved public infrastructure project.
///
/// Contributions are accepted only while the project is in Funding, are capped
/// at a provisional share of project cost, and never alter construction duration.
pub fn contribute_to_infrastructure_project(
    state: &mut GameState,
    project_id: InfrastructureProjectId,
    amount: Money,
    now: UtcSeconds,
) -> Result<(), InfrastructureProjectActionError> {
    if amount <= Money::ZERO {
        return Err(InfrastructureProjectActionError::InvalidContribution);
    }

    let project_index = state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .position(|project| project.id == project_id)
        .ok_or(InfrastructureProjectActionError::ProjectNotFound { project_id })?;

    let target_name = project_target_settlement_name(&state.region, project_index);
    let project = &state.region.rail_authority.infrastructure_projects[project_index];
    if project.status != InfrastructureProjectStatus::Funding {
        return Err(InfrastructureProjectActionError::CannotContribute {
            project_id,
            status: project.status,
        });
    }
    if amount > project.funding.funding_gap()? {
        return Err(InfrastructureProjectActionError::ContributionExceedsFundingGap);
    }
    if amount > project.funding.remaining_operator_contribution_capacity()? {
        return Err(InfrastructureProjectActionError::ContributionExceedsOperatorCap);
    }
    if amount > state.player_company.funds {
        return Err(InfrastructureProjectActionError::InsufficientCompanyFunds);
    }

    let funds_after = state.player_company.funds.checked_sub(amount)?;
    let project = &mut state.region.rail_authority.infrastructure_projects[project_index];
    project.funding.operator_contributed =
        project.funding.operator_contributed.checked_add(amount)?;
    let funding_completed =
        project.funding.is_fully_funded() && project.timeline.funding_completed_at.is_none();
    if funding_completed {
        project.timeline.funding_completed_at = Some(now);
    }
    state.player_company.funds = funds_after;
    if funding_completed {
        push_bulletin(
            &mut state.region,
            now,
            BulletinCategory::Authority,
            format!("Funding secured for {target_name} connection"),
            "The public funding gap is closed and the project can wait for a construction slot."
                .into(),
        );
    }
    Ok(())
}

/// Advances the complete Rail Authority lifecycle up to `now`.
///
/// Callers provide world context, but the Authority owns the ordering of its
/// fiscal, planning, funding, scheduling, construction, and opening stages.
pub fn advance_rail_authority(
    region: &mut Region,
    world_seed: u64,
    demand: &[OriginDestinationDemand],
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_authority_fiscal_periods(region, now)?;
    advance_infrastructure_planning_with_rules(region, world_seed, demand, authority_rules, now)?;
    advance_project_funding(region, now)?;
    advance_project_scheduling_with_rules(region, authority_rules, now)?;
    advance_project_construction_with_rules(region, authority_rules, now)?;
    open_completed_infrastructure_projects(region, now)?;
    Ok(())
}

/// Advances the Rail Authority's daily UTC fiscal calendar.
///
/// At each midnight the previous maintenance reserve is paid, remaining
/// uncommitted investment carries forward, the recurring regional public
/// allocation is deposited, and maintenance is reserved for the new day.
/// Multiple missed calendar days are processed when RailQ is reopened.
pub(crate) fn advance_authority_fiscal_periods(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<u32, CalculationError> {
    let authority = &mut region.rail_authority;
    if authority.finances.next_fiscal_period_at.is_none() {
        authority.finances.initialize_fiscal_calendar(now)?;
        return Ok(0);
    }

    let mut processed = 0_u32;
    loop {
        let Some(next_period) = authority.finances.next_fiscal_period_at else {
            break;
        };
        if next_period > now {
            break;
        }

        if processed >= MAX_OFFLINE_FISCAL_CATCHUP_PERIODS {
            authority.finances.next_fiscal_period_at =
                Some(crate::model::next_utc_midnight_after(now)?);
            break;
        }

        let maintenance_paid = authority.finances.maintenance_reserve;
        authority.finances.treasury = authority.finances.treasury.checked_sub(maintenance_paid)?;
        authority.finances.maintenance_reserve = Money::ZERO;
        authority.finances.carried_over_funds = authority.finances.uncommitted_investment()?;
        authority.finances.receive_regional_public_allocation()?;
        authority
            .finances
            .refresh_maintenance_reserve(&authority.rail_network)?;
        authority.finances.next_fiscal_period_at =
            Some(crate::model::next_utc_midnight_after(next_period)?);
        processed = processed.checked_add(1).ok_or(CalculationError::Overflow {
            operation: "Rail Authority fiscal period count",
        })?;
    }

    Ok(processed)
}

/// Advances the early planning lifecycle and opens the next highest-ranked
/// connection request when the planning desk is free.
///
/// Only one New Line project is actively moving through Requested/UnderReview/
/// Proposed at a time. The persisted expansion-pipeline limit also prevents
/// approved/funded work from letting the Authority queue the whole world at once.
pub(crate) fn advance_infrastructure_planning(
    region: &mut Region,
    world_seed: u64,
    demand: &[OriginDestinationDemand],
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_infrastructure_planning_with_rules(
        region,
        world_seed,
        demand,
        &AuthorityRules::provisional(),
        now,
    )
}

fn advance_infrastructure_planning_with_rules(
    region: &mut Region,
    world_seed: u64,
    demand: &[OriginDestinationDemand],
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_existing_planning_projects(region, world_seed, authority_rules, now)?;

    if region
        .rail_authority
        .infrastructure_projects
        .iter()
        .any(|project| is_active_planning_status(project.status))
    {
        return Ok(());
    }

    if active_expansion_project_count(region) >= authority_rules.max_active_expansion_projects() {
        return Ok(());
    }

    if connection_request_cooldown_active(region, authority_rules, now)? {
        return Ok(());
    }

    // Reconsider one previously Deferred council request before accepting a
    // brand-new one. Reconsideration uses the same project identity, requires
    // stronger local rail adoption each time, and resets only the review-stage
    // timestamps. Funding/construction history is never rewritten here.
    if let Some(index) =
        deferred_project_ready_for_reconsideration(region, demand, authority_rules, now)?
    {
        let target_name = project_target_settlement_name(region, index);
        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.status = InfrastructureProjectStatus::Requested;
        project.timeline.requested_at = now;
        project.timeline.review_started_at = None;
        project.timeline.proposed_at = None;
        project.timeline.approved_at = None;
        project.timeline.deferred_at = None;
        project.timeline.reconsideration_count =
            project.timeline.reconsideration_count.saturating_add(1);
        push_bulletin(
            region,
            now,
            BulletinCategory::Local,
            format!("{target_name} Council renews rail connection request"),
            "Stronger nearby rail adoption has reopened the case for a connection.".into(),
        );
        return Ok(());
    }

    // Councils request connections only after rail travel around the nearest
    // existing Station has become established through successful passenger
    // operation. Candidate scoring still decides which eligible request the
    // Authority planning desk takes first.
    let candidates = evaluate_connection_candidates(region, world_seed)?;
    let next_candidate = candidates.into_iter().find(|candidate| {
        !region
            .rail_authority
            .infrastructure_projects
            .iter()
            .any(|project| project_targets_settlement(project, candidate.settlement_id))
            && local_rail_success_basis_points(demand, candidate.connection_station_id)
                >= COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS
    });

    if let Some(candidate) = next_candidate {
        let target_name = region
            .settlements
            .iter()
            .find(|settlement| settlement.id == candidate.settlement_id)
            .map(|settlement| settlement.name.clone())
            .unwrap_or_else(|| "Regional".into());
        region
            .rail_authority
            .infrastructure_projects
            .push(project_from_candidate(candidate, now));
        push_bulletin(
            region,
            now,
            BulletinCategory::Local,
            format!("{target_name} Council requests railway connection"),
            "Growing use of the nearby railway has strengthened the local case for joining the network."
                .into(),
        );
    }

    Ok(())
}

fn connection_request_cooldown_active(
    region: &Region,
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<bool, CalculationError> {
    let latest_request = region
        .rail_authority
        .infrastructure_projects
        .iter()
        .filter(|project| matches!(&project.kind, InfrastructureProjectKind::NewLine { .. }))
        .map(|project| project.timeline.requested_at)
        .max();

    let Some(latest_request) = latest_request else {
        return Ok(false);
    };
    Ok(latest_request.checked_add(authority_rules.council_request_cooldown())? > now)
}

fn deferred_project_ready_for_reconsideration(
    region: &Region,
    demand: &[OriginDestinationDemand],
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<Option<usize>, CalculationError> {
    let mut candidates = region
        .rail_authority
        .infrastructure_projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            if project.status != InfrastructureProjectStatus::Deferred {
                return None;
            }
            let deferred_at = project.timeline.deferred_at?;
            let connection_station_id = project_connection_station_id(project)?;
            Some((
                index,
                deferred_at,
                connection_station_id,
                project.timeline.reconsideration_count,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(index, deferred_at, _, _)| (*deferred_at, *index));

    for (index, deferred_at, connection_station_id, reconsideration_count) in candidates {
        if deferred_at.checked_add(authority_rules.deferred_reconsideration_delay())? > now {
            continue;
        }
        let required = deferred_reconsideration_threshold(reconsideration_count);
        if local_rail_success_basis_points(demand, connection_station_id) >= required {
            return Ok(Some(index));
        }
    }

    Ok(None)
}

pub(crate) fn deferred_reconsideration_threshold(reconsideration_count: u8) -> u16 {
    DEFERRED_RECONSIDERATION_BASE_MATURITY_BASIS_POINTS
        .saturating_add(
            DEFERRED_RECONSIDERATION_STEP_BASIS_POINTS
                .saturating_mul(u16::from(reconsideration_count)),
        )
        .min(10_001)
}

/// Returns the strongest established bidirectional passenger market touching
/// `station_id`, expressed in the same basis points as market maturity.
///
/// Using the strongest local corridor avoids diluting a successful Station as
/// the wider network grows, while averaging both directions prevents one busy
/// direction alone from immediately triggering political pressure.
pub fn local_rail_success_basis_points(
    demand: &[OriginDestinationDemand],
    station_id: RailStationId,
) -> u16 {
    let mut counterpart_ids = BTreeSet::new();
    for pool in demand {
        if pool.origin_station_id == station_id {
            counterpart_ids.insert(pool.destination_station_id);
        } else if pool.destination_station_id == station_id {
            counterpart_ids.insert(pool.origin_station_id);
        }
    }

    counterpart_ids
        .into_iter()
        .filter_map(|counterpart_id| {
            let forward = demand.iter().find(|pool| {
                pool.origin_station_id == station_id
                    && pool.destination_station_id == counterpart_id
            });
            let reverse = demand.iter().find(|pool| {
                pool.origin_station_id == counterpart_id
                    && pool.destination_station_id == station_id
            });

            match (forward, reverse) {
                (Some(forward), Some(reverse)) => Some(
                    (u32::from(forward.market_maturity.basis_points())
                        + u32::from(reverse.market_maturity.basis_points()))
                        / 2,
                ),
                (Some(pool), None) | (None, Some(pool)) => {
                    Some(u32::from(pool.market_maturity.basis_points()))
                }
                (None, None) => None,
            }
        })
        .max()
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0)
}

/// Moves approved projects into the active Funding pipeline and commits
/// currently available Authority investment funds in approval order. Partial
/// funding is allowed, but only a small number of projects are actively funded
/// at once; later approved projects remain Approved until a funding slot opens.
pub(crate) fn advance_project_funding(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    let before = region
        .rail_authority
        .infrastructure_projects
        .iter()
        .map(|project| (project.status, project.timeline.funding_completed_at))
        .collect::<Vec<_>>();
    let authority = &mut region.rail_authority;
    let funding_capacity = authority.funding_pipeline_capacity();

    // Older saves may already contain many zero-funded projects in Funding
    // from the previous unlimited pipeline. Return only untouched excess
    // projects to Approved; never demote anything that already received public
    // or operator money.
    let mut existing_funding = authority
        .infrastructure_projects
        .iter()
        .enumerate()
        .filter(|(_, project)| project.status == InfrastructureProjectStatus::Funding)
        .map(|(index, project)| {
            (
                index,
                project
                    .timeline
                    .approved_at
                    .unwrap_or(project.timeline.requested_at),
                project.id,
            )
        })
        .collect::<Vec<_>>();
    existing_funding.sort_by_key(|(_, approved_at, id)| (*approved_at, *id));

    let mut retained = 0_u32;
    for (index, _, _) in existing_funding {
        let project = &authority.infrastructure_projects[index];
        let has_funding = project.funding.authority_committed > Money::ZERO
            || project.funding.operator_contributed > Money::ZERO
            || project.timeline.funding_completed_at.is_some();
        if retained < funding_capacity || has_funding {
            retained = retained.saturating_add(1);
        } else {
            authority.infrastructure_projects[index].status = InfrastructureProjectStatus::Approved;
        }
    }

    let mut active_funding = authority.active_funding_count();
    let mut indices = (0..authority.infrastructure_projects.len()).collect::<Vec<_>>();
    indices.sort_by_key(|&index| {
        let project = &authority.infrastructure_projects[index];
        (
            project
                .timeline
                .approved_at
                .unwrap_or(project.timeline.requested_at),
            project.id,
        )
    });

    for index in indices {
        if authority.infrastructure_projects[index].status == InfrastructureProjectStatus::Approved
        {
            if active_funding >= funding_capacity {
                continue;
            }
            authority.infrastructure_projects[index].status = InfrastructureProjectStatus::Funding;
            active_funding = active_funding.saturating_add(1);
        }
        if authority.infrastructure_projects[index].status != InfrastructureProjectStatus::Funding {
            continue;
        }
        if authority.infrastructure_projects[index]
            .funding
            .is_fully_funded()
        {
            authority.infrastructure_projects[index]
                .timeline
                .funding_completed_at
                .get_or_insert(now);
            continue;
        }

        let available = authority.finances.uncommitted_investment()?;
        if available <= Money::ZERO {
            continue;
        }
        let gap = authority.infrastructure_projects[index]
            .funding
            .funding_gap()?;
        let commitment = available.min(gap);
        authority.finances.committed_investment = authority
            .finances
            .committed_investment
            .checked_add(commitment)?;

        let project = &mut authority.infrastructure_projects[index];
        project.funding.authority_committed = project
            .funding
            .authority_committed
            .checked_add(commitment)?;
        if project.funding.is_fully_funded() && project.timeline.funding_completed_at.is_none() {
            project.timeline.funding_completed_at = Some(now);
        }
    }

    for (index, (previous_status, previous_funded_at)) in before.into_iter().enumerate() {
        let Some(project) = region.rail_authority.infrastructure_projects.get(index) else {
            continue;
        };
        let status = project.status;
        let funding_completed_at = project.timeline.funding_completed_at;
        let target_name = project_target_settlement_name(region, index);
        if previous_status == InfrastructureProjectStatus::Approved
            && status == InfrastructureProjectStatus::Funding
        {
            push_bulletin(
                region,
                now,
                BulletinCategory::Authority,
                format!("{target_name} connection enters public funding"),
                "The approved project has entered the Rail Authority investment pipeline.".into(),
            );
        }
        if previous_funded_at.is_none() {
            if let Some(funded_at) = funding_completed_at {
                push_bulletin(
                    region,
                    funded_at,
                    BulletinCategory::Authority,
                    format!("Funding secured for {target_name} connection"),
                    "The funding gap is closed and the project can wait for construction capacity."
                        .into(),
                );
            }
        }
    }

    Ok(())
}

/// Reserves available construction capacity for fully funded projects in
/// funding-completion order. Projects that cannot reserve a slot remain in
/// `Funding`, even when their financial gap is already zero.
pub(crate) fn advance_project_scheduling(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_project_scheduling_with_rules(region, &AuthorityRules::provisional(), now)
}

fn advance_project_scheduling_with_rules(
    region: &mut Region,
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    loop {
        let next_index = {
            let authority = &region.rail_authority;
            let mut candidates = authority
                .infrastructure_projects
                .iter()
                .enumerate()
                .filter(|(_, project)| {
                    project.status == InfrastructureProjectStatus::Funding
                        && project.funding.is_fully_funded()
                        && project.timeline.funding_completed_at.is_some()
                        && authority.can_schedule_construction(project)
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|(_, project)| {
                (
                    project
                        .timeline
                        .funding_completed_at
                        .or(project.timeline.approved_at)
                        .unwrap_or(project.timeline.requested_at),
                    project.id,
                )
            });
            candidates.first().map(|(index, _)| *index)
        };

        let Some(index) = next_index else {
            break;
        };

        let scheduled_start = now.checked_add(authority_rules.construction_mobilisation_delay())?;
        let target_name = project_target_settlement_name(region, index);
        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.status = InfrastructureProjectStatus::Scheduled;
        project.timeline.scheduled_start_at = Some(scheduled_start);
        push_bulletin(
            region,
            now,
            BulletinCategory::Construction,
            format!("Construction scheduled for {target_name} connection"),
            "The funded project has secured construction capacity and entered mobilisation.".into(),
        );
    }

    Ok(())
}

/// Starts due New Line projects and locks in their physical completion time.
///
/// Reconciliation uses the scheduled start timestamp rather than `now`, so a
/// project that became due while RailQ was closed keeps the same construction
/// duration it would have had while the game was open. Once written, the
/// planned completion timestamp is never recomputed by funding changes.
pub(crate) fn advance_project_construction(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_project_construction_with_rules(region, &AuthorityRules::provisional(), now)
}

fn advance_project_construction_with_rules(
    region: &mut Region,
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    for index in 0..region.rail_authority.infrastructure_projects.len() {
        let project = &region.rail_authority.infrastructure_projects[index];
        if project.status != InfrastructureProjectStatus::Scheduled {
            continue;
        }
        let InfrastructureProjectKind::NewLine { .. } = &project.kind else {
            // Other project kinds receive construction behaviour in their own
            // modernization batches.
            continue;
        };
        let Some(scheduled_start) = project.timeline.scheduled_start_at else {
            continue;
        };
        if scheduled_start > now {
            continue;
        }

        let duration = new_line_construction_duration_with_rules(project, authority_rules)?;
        let planned_completion = scheduled_start.checked_add(duration)?;
        let target_name = project_target_settlement_name(region, index);
        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.status = InfrastructureProjectStatus::Construction;
        project.timeline.construction_started_at = Some(scheduled_start);
        project.timeline.planned_completion_at = Some(planned_completion);
        push_bulletin(
            region,
            scheduled_start,
            BulletinCategory::Construction,
            format!("Construction begins on {target_name} connection"),
            "Rail Authority crews have started physical work on the new railway connection.".into(),
        );
    }

    Ok(())
}

/// Opens completed New Line projects and materialises their approved Stations
/// and physical Rail Lines into the public network.
///
/// Completion uses the project's locked `planned_completion_at` timestamp so
/// reopening RailQ after an offline gap produces the same opening time as
/// keeping the game running. Authority commitments become spent public money
/// at opening; the historical project keeps its original funding record.
pub(crate) fn open_completed_infrastructure_projects(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    let due_indices = region
        .rail_authority
        .infrastructure_projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            if project.status != InfrastructureProjectStatus::Construction {
                return None;
            }
            let completion = project.timeline.planned_completion_at?;
            (completion <= now).then_some(index)
        })
        .collect::<Vec<_>>();

    for index in due_indices {
        let (planned_stations, planned_lines, completion, authority_committed) = {
            let project = &region.rail_authority.infrastructure_projects[index];
            let InfrastructureProjectKind::NewLine {
                planned_stations,
                planned_lines,
            } = &project.kind
            else {
                continue;
            };

            (
                planned_stations.clone(),
                planned_lines.clone(),
                project
                    .timeline
                    .planned_completion_at
                    .expect("due construction project has a completion timestamp"),
                project.funding.authority_committed,
            )
        };

        {
            let network = &mut region.rail_authority.rail_network;
            for station in planned_stations {
                if !network
                    .rail_stations
                    .iter()
                    .any(|existing| existing.id == station.id)
                {
                    network.rail_stations.push(RailStation {
                        id: station.id,
                        settlement_id: station.settlement_id,
                    });
                }
            }

            for line in planned_lines {
                if !network
                    .rail_lines
                    .iter()
                    .any(|existing| existing.id == line.id)
                {
                    network.rail_lines.push(RailLine {
                        id: line.id,
                        first_station_id: line.first_station_id,
                        second_station_id: line.second_station_id,
                        distance: line.distance,
                        speed_limit: line.speed_limit,
                        track_count: line.track_count,
                        electrification: line.electrification,
                        construction_difficulty: line.construction_difficulty,
                    });
                }
            }
        }

        region.rail_authority.finances.treasury = region
            .rail_authority
            .finances
            .treasury
            .checked_sub(authority_committed)?;
        region.rail_authority.finances.committed_investment = region
            .rail_authority
            .finances
            .committed_investment
            .checked_sub(authority_committed)?;
        region
            .rail_authority
            .finances
            .refresh_maintenance_reserve(&region.rail_authority.rail_network)?;

        let target_name = project_target_settlement_name(region, index);
        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.funding.award_operator_access_credit()?;
        project.status = InfrastructureProjectStatus::Open;
        project.timeline.completed_at = Some(completion);
        push_bulletin(
            region,
            completion,
            BulletinCategory::Network,
            format!("{target_name} joins the rail network"),
            "The new public railway connection and station are open for passenger operations."
                .into(),
        );
    }

    Ok(())
}

fn new_line_construction_duration(
    project: &InfrastructureProject,
) -> Result<DurationSeconds, CalculationError> {
    new_line_construction_duration_with_rules(project, &AuthorityRules::provisional())
}

fn new_line_construction_duration_with_rules(
    project: &InfrastructureProject,
    authority_rules: &AuthorityRules,
) -> Result<DurationSeconds, CalculationError> {
    let InfrastructureProjectKind::NewLine { planned_lines, .. } = &project.kind else {
        return Ok(DurationSeconds::from_seconds(0));
    };

    let mut seconds = authority_rules
        .new_line_base_construction_duration()
        .seconds();
    for line in planned_lines {
        let seconds_per_kilometre = match line.construction_difficulty {
            ConstructionDifficulty::Low => authority_rules.low_difficulty_seconds_per_kilometre(),
            ConstructionDifficulty::Moderate => {
                authority_rules.moderate_difficulty_seconds_per_kilometre()
            }
            ConstructionDifficulty::High => authority_rules.high_difficulty_seconds_per_kilometre(),
        };
        let metre_seconds = line
            .distance
            .metres()
            .checked_mul(seconds_per_kilometre)
            .ok_or(CalculationError::Overflow {
                operation: "infrastructure construction duration",
            })?;
        let line_seconds = metre_seconds / 1_000;
        let line_seconds = if metre_seconds % 1_000 == 0 {
            line_seconds
        } else {
            line_seconds
                .checked_add(1)
                .ok_or(CalculationError::Overflow {
                    operation: "infrastructure construction duration",
                })?
        };
        seconds = seconds
            .checked_add(line_seconds)
            .ok_or(CalculationError::Overflow {
                operation: "infrastructure construction duration",
            })?;
    }

    Ok(DurationSeconds::from_seconds(seconds))
}

fn advance_existing_planning_projects(
    region: &mut Region,
    world_seed: u64,
    authority_rules: &AuthorityRules,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    for index in 0..region.rail_authority.infrastructure_projects.len() {
        loop {
            let status = region.rail_authority.infrastructure_projects[index].status;
            match status {
                InfrastructureProjectStatus::Requested => {
                    let due = region.rail_authority.infrastructure_projects[index]
                        .timeline
                        .requested_at
                        .checked_add(authority_rules.request_queue_delay())?;
                    if due > now {
                        break;
                    }
                    let target_name = project_target_settlement_name(region, index);
                    let project = &mut region.rail_authority.infrastructure_projects[index];
                    project.status = InfrastructureProjectStatus::UnderReview;
                    project.timeline.review_started_at = Some(due);
                    push_bulletin(
                        region,
                        due,
                        BulletinCategory::Authority,
                        format!("Rail Authority opens review of {target_name} connection"),
                        "The council request has entered formal regional infrastructure review."
                            .into(),
                    );
                }
                InfrastructureProjectStatus::UnderReview => {
                    let project = &region.rail_authority.infrastructure_projects[index];
                    let started_at = project
                        .timeline
                        .review_started_at
                        .unwrap_or(project.timeline.requested_at);
                    let due = started_at.checked_add(authority_rules.review_duration())?;
                    if due > now {
                        break;
                    }
                    let target_name = project_target_settlement_name(region, index);
                    let project = &mut region.rail_authority.infrastructure_projects[index];
                    project.timeline.review_started_at.get_or_insert(started_at);
                    project.status = InfrastructureProjectStatus::Proposed;
                    project.timeline.proposed_at = Some(due);
                    push_bulletin(
                        region,
                        due,
                        BulletinCategory::Authority,
                        format!("{target_name} connection proposal prepared"),
                        "The Rail Authority has completed its initial review and prepared the project for a decision.".into(),
                    );
                }
                InfrastructureProjectStatus::Proposed => {
                    let project = &region.rail_authority.infrastructure_projects[index];
                    let proposed_at = project
                        .timeline
                        .proposed_at
                        .or(project.timeline.review_started_at)
                        .unwrap_or(project.timeline.requested_at);
                    let due = proposed_at.checked_add(authority_rules.proposal_duration())?;
                    if due > now {
                        break;
                    }

                    let approval_score = project_review_score(
                        region,
                        &region.rail_authority.infrastructure_projects[index],
                        world_seed,
                    );
                    let target_name = project_target_settlement_name(region, index);
                    let approved = approval_score
                        .is_some_and(|score| score >= AUTHORITY_APPROVAL_SCORE_THRESHOLD);
                    let project = &mut region.rail_authority.infrastructure_projects[index];
                    project.timeline.proposed_at.get_or_insert(proposed_at);
                    if approved {
                        project.status = InfrastructureProjectStatus::Approved;
                        project.timeline.approved_at = Some(due);
                    } else {
                        project.status = InfrastructureProjectStatus::Deferred;
                        project.timeline.deferred_at = Some(due);
                    }
                    if approved {
                        push_bulletin(
                            region,
                            due,
                            BulletinCategory::Authority,
                            format!("Rail Authority approves {target_name} connection"),
                            "The regional case has passed review and the project can enter the public funding pipeline.".into(),
                        );
                    } else {
                        push_bulletin(
                            region,
                            due,
                            BulletinCategory::Authority,
                            format!("Rail Authority defers {target_name} connection"),
                            "The current regional case does not yet justify the project cost; stronger rail adoption can trigger reconsideration.".into(),
                        );
                    }
                }
                _ => break,
            }
        }
    }
    Ok(())
}

pub(crate) fn project_connection_station_id(
    project: &InfrastructureProject,
) -> Option<RailStationId> {
    let InfrastructureProjectKind::NewLine {
        planned_stations,
        planned_lines,
    } = &project.kind
    else {
        return None;
    };
    let planned_station = planned_stations.first()?;
    let planned_line = planned_lines.iter().find(|line| {
        line.first_station_id == planned_station.id || line.second_station_id == planned_station.id
    })?;
    Some(if planned_line.first_station_id == planned_station.id {
        planned_line.second_station_id
    } else {
        planned_line.first_station_id
    })
}

fn active_expansion_project_count(region: &Region) -> u32 {
    u32::try_from(
        region
            .rail_authority
            .infrastructure_projects
            .iter()
            .filter(|project| {
                matches!(&project.kind, InfrastructureProjectKind::NewLine { .. })
                    && matches!(
                        project.status,
                        InfrastructureProjectStatus::Requested
                            | InfrastructureProjectStatus::UnderReview
                            | InfrastructureProjectStatus::Proposed
                            | InfrastructureProjectStatus::Approved
                            | InfrastructureProjectStatus::Funding
                            | InfrastructureProjectStatus::Scheduled
                            | InfrastructureProjectStatus::Construction
                    )
            })
            .count(),
    )
    .unwrap_or(u32::MAX)
}

pub(crate) fn project_review_score(
    region: &Region,
    project: &InfrastructureProject,
    world_seed: u64,
) -> Option<i32> {
    let InfrastructureProjectKind::NewLine {
        planned_stations, ..
    } = &project.kind
    else {
        return None;
    };
    let planned_station = planned_stations.first()?;
    let connection_station_id = project_connection_station_id(project)?;

    Some(
        score_candidate(
            region,
            planned_station.settlement_id,
            connection_station_id,
            project.funding.estimated_cost,
            world_seed,
        )
        .total,
    )
}

fn is_active_planning_status(status: InfrastructureProjectStatus) -> bool {
    matches!(
        status,
        InfrastructureProjectStatus::Requested
            | InfrastructureProjectStatus::UnderReview
            | InfrastructureProjectStatus::Proposed
    )
}

fn project_targets_settlement(
    project: &InfrastructureProject,
    settlement_id: SettlementId,
) -> bool {
    if project.status == InfrastructureProjectStatus::Cancelled {
        return false;
    }

    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations, ..
        } => planned_stations
            .iter()
            .any(|station| station.settlement_id == settlement_id),
        _ => false,
    }
}

fn project_from_candidate(
    candidate: ConnectionCandidate,
    requested_at: UtcSeconds,
) -> InfrastructureProject {
    let planned_station_id = RailStationId::new_v4();
    let planned_line_id = RailLineId::new_v4();

    InfrastructureProject {
        id: InfrastructureProjectId::new_v4(),
        kind: InfrastructureProjectKind::NewLine {
            planned_stations: vec![PlannedRailStation {
                id: planned_station_id,
                settlement_id: candidate.settlement_id,
            }],
            planned_lines: vec![PlannedRailLine {
                id: planned_line_id,
                first_station_id: candidate.connection_station_id,
                second_station_id: planned_station_id,
                distance: candidate.estimated_distance,
                speed_limit: SpeedKilometresPerHour::new(70)
                    .expect("RailQ's provisional New Line speed limit is valid"),
                track_count: TrackCount::SINGLE,
                electrification: Electrification::None,
                construction_difficulty: candidate.construction_difficulty,
            }],
        },
        status: InfrastructureProjectStatus::Requested,
        timeline: InfrastructureProjectTimeline {
            requested_at,
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
        },
        funding: InfrastructureProjectFunding {
            estimated_cost: candidate.estimated_cost,
            authority_committed: Money::ZERO,
            operator_contributed: Money::ZERO,
            access_fee_credit_awarded: Money::ZERO,
            access_fee_credit_remaining: Money::ZERO,
        },
    }
}

/// Evaluates and ranks every currently unconnected Settlement.
///
/// RailQ does not yet store geographical coordinates. Until that arrives,
/// proximity and construction difficulty are stable seeded estimates derived
/// from the world seed and entity identities. Keeping this logic isolated makes
/// it straightforward to replace with real map geometry later without changing
/// the Authority project model.
pub fn evaluate_connection_candidates(
    region: &Region,
    world_seed: u64,
) -> Result<Vec<ConnectionCandidate>, CalculationError> {
    let network = &region.rail_authority.rail_network;
    if network.rail_stations.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = region
        .settlements
        .iter()
        .filter(|settlement| {
            !network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
        })
        .map(|settlement| {
            let (connection_station_id, estimated_distance) = network
                .rail_stations
                .iter()
                .map(|station| {
                    (
                        station.id,
                        geographic_connection_distance(region, settlement.id, station.id),
                    )
                })
                .min_by_key(|(station_id, distance)| (*distance, *station_id))
                .expect("non-empty Rail Network has at least one Station");

            let construction_difficulty =
                estimated_construction_difficulty(world_seed, settlement.id, connection_station_id);
            let estimated_cost =
                estimated_connection_cost(estimated_distance, construction_difficulty)?;
            let score = score_candidate(
                region,
                settlement.id,
                connection_station_id,
                estimated_cost,
                world_seed,
            );

            Ok(ConnectionCandidate {
                settlement_id: settlement.id,
                connection_station_id,
                estimated_distance,
                construction_difficulty,
                estimated_cost,
                score,
            })
        })
        .collect::<Result<Vec<_>, CalculationError>>()?;

    candidates.sort_by(|left, right| {
        right
            .score
            .total
            .cmp(&left.score.total)
            .then_with(|| left.estimated_cost.cmp(&right.estimated_cost))
            .then_with(|| left.settlement_id.cmp(&right.settlement_id))
    });

    Ok(candidates)
}

pub(crate) fn nearest_connection_station_id(
    region: &Region,
    settlement_id: SettlementId,
) -> Option<RailStationId> {
    region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| {
            (
                station.id,
                geographic_connection_distance(region, settlement_id, station.id),
            )
        })
        .min_by_key(|(station_id, distance)| (*distance, *station_id))
        .map(|(station_id, _)| station_id)
}

fn geographic_connection_distance(
    region: &Region,
    settlement_id: SettlementId,
    station_id: RailStationId,
) -> DistanceMetres {
    let settlement = region
        .settlements
        .iter()
        .find(|settlement| settlement.id == settlement_id)
        .expect("connection candidate Settlement belongs to the Region");
    let station = region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
        .expect("connection candidate Rail Station belongs to the Region");
    let station_settlement = region
        .settlements
        .iter()
        .find(|candidate| candidate.id == station.settlement_id)
        .expect("Rail Station Settlement belongs to the Region");

    let dx = i64::from(settlement.position.x) - i64::from(station_settlement.position.x);
    let dy = i64::from(settlement.position.y) - i64::from(station_settlement.position.y);
    let squared_km = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
    let kilometres = (squared_km as f64).sqrt().ceil().max(1.0) as i64;
    DistanceMetres::new(kilometres.saturating_mul(1_000))
        .expect("distinct Settlement coordinates produce a positive connection distance")
}

fn estimated_construction_difficulty(
    world_seed: u64,
    settlement_id: SettlementId,
    station_id: RailStationId,
) -> ConstructionDifficulty {
    match mix_seed(
        world_seed.rotate_left(11)
            ^ settlement_id.get().rotate_left(29)
            ^ station_id.get().rotate_left(47),
    ) % 100
    {
        0..=44 => ConstructionDifficulty::Low,
        45..=84 => ConstructionDifficulty::Moderate,
        _ => ConstructionDifficulty::High,
    }
}

fn estimated_connection_cost(
    distance: DistanceMetres,
    difficulty: ConstructionDifficulty,
) -> Result<Money, CalculationError> {
    let cents_per_kilometre = match difficulty {
        ConstructionDifficulty::Low => 120_000,
        ConstructionDifficulty::Moderate => 160_000,
        ConstructionDifficulty::High => 220_000,
    };
    MoneyPerKilometre::new(cents_per_kilometre)
        .expect("provisional construction rate is positive")
        .checked_charge(distance)
}

fn score_candidate(
    region: &Region,
    settlement_id: SettlementId,
    connection_station_id: RailStationId,
    estimated_cost: Money,
    world_seed: u64,
) -> ConnectionCandidateScore {
    let settlement = region
        .settlements
        .iter()
        .find(|settlement| settlement.id == settlement_id)
        .expect("candidate Settlement belongs to the Region");

    let connected_settlement_count = region.rail_authority.rail_network.rail_stations.len() as u32;
    let connection_degree = region
        .rail_authority
        .rail_network
        .rail_lines
        .iter()
        .filter(|line| {
            line.first_station_id == connection_station_id
                || line.second_station_id == connection_station_id
        })
        .count() as u32;

    let population = u32::try_from((settlement.population / 1_000).min(300)).unwrap_or(300);
    let latent_demand = u32::try_from(
        (settlement.population / 5_000)
            .saturating_add(u64::from(connected_settlement_count) * 8)
            .min(200),
    )
    .unwrap_or(200);
    let network_usefulness = 25_u32
        .saturating_add(connection_degree.saturating_mul(35))
        .saturating_add(connected_settlement_count.saturating_mul(5))
        .min(175);
    let regional_development = 30
        + u32::try_from(
            mix_seed(world_seed ^ settlement_id.get().rotate_left(7) ^ 0x5245_4749_4f4e_414c) % 121,
        )
        .expect("regional-development score fits u32");

    // €/$1,000 of provisional project cost costs one score point, capped so
    // expensive but strategically valuable connections remain possible.
    let non_negative_cost_cents = estimated_cost.cents().max(0) as u64;
    let construction_cost_penalty =
        u32::try_from((non_negative_cost_cents / 100_000).min(250)).unwrap_or(250);

    let positive = population
        .saturating_add(latent_demand)
        .saturating_add(network_usefulness)
        .saturating_add(regional_development);
    let total = i32::try_from(positive).unwrap_or(i32::MAX)
        - i32::try_from(construction_cost_penalty).unwrap_or(i32::MAX);

    ConnectionCandidateScore {
        population,
        latent_demand,
        network_usefulness,
        regional_development,
        construction_cost_penalty,
        total,
    }
}

fn mix_seed(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{
        model::{
            AuthorityRules, BulletinCategory, ConstructionDifficulty, DistanceMetres,
            DurationSeconds, InfrastructureProjectId, InfrastructureProjectKind,
            InfrastructureProjectStatus, MarketMaturity, Money, OriginDestinationDemand,
            UtcSeconds,
        },
        sim::{
            authority::MAX_OFFLINE_FISCAL_CATCHUP_PERIODS,
            demand::seed_directional_demand,
            world::{create_new_game, generate_region},
        },
    };

    use super::{
        InfrastructureProjectActionError, advance_authority_fiscal_periods,
        advance_infrastructure_planning, advance_infrastructure_planning_with_rules,
        advance_project_construction, advance_project_construction_with_rules,
        advance_project_funding, advance_project_scheduling, advance_project_scheduling_with_rules,
        cancel_infrastructure_project, contribute_to_infrastructure_project,
        deferred_reconsideration_threshold, estimated_connection_cost,
        evaluate_connection_candidates, local_rail_success_basis_points,
        new_line_construction_duration, new_line_construction_duration_with_rules,
        open_completed_infrastructure_projects, project_from_candidate,
    };

    fn fully_mature_demand(
        region: &crate::model::Region,
        world_seed: u64,
    ) -> Vec<OriginDestinationDemand> {
        let mut demand = seed_directional_demand(region, world_seed);
        for pool in &mut demand {
            pool.market_maturity = MarketMaturity::full();
        }
        demand
    }

    #[test]
    fn fiscal_period_deposits_public_allocation_and_advances_schedule() {
        let started = UtcSeconds::from_unix_seconds(10_000);
        let mut state = create_new_game(42, "One More Prime", started);
        let allocation = state
            .region
            .rail_authority
            .finances
            .regional_public_allocation;
        let treasury_before = state.region.rail_authority.finances.treasury;
        let maintenance_paid = state.region.rail_authority.finances.maintenance_reserve;
        let next = state
            .region
            .rail_authority
            .finances
            .next_fiscal_period_at
            .unwrap();
        assert_eq!(next, UtcSeconds::from_unix_seconds(86_400));

        let processed = advance_authority_fiscal_periods(&mut state.region, next).unwrap();

        assert_eq!(processed, 1);
        assert_eq!(
            state.region.rail_authority.finances.treasury,
            treasury_before
                .checked_sub(maintenance_paid)
                .unwrap()
                .checked_add(allocation)
                .unwrap()
        );
        assert!(state.region.rail_authority.finances.carried_over_funds > Money::ZERO);
        assert_eq!(
            state.region.rail_authority.finances.next_fiscal_period_at,
            Some(UtcSeconds::from_unix_seconds(
                next.unix_seconds() + 24 * 60 * 60
            ))
        );
    }

    #[test]
    fn fiscal_calendar_catches_up_missed_midnights_after_offline_time() {
        let started = UtcSeconds::from_unix_seconds(10_000);
        let mut state = create_new_game(42, "One More Prime", started);

        let processed = advance_authority_fiscal_periods(
            &mut state.region,
            UtcSeconds::from_unix_seconds(3 * 86_400),
        )
        .unwrap();

        assert_eq!(processed, 3);
        assert_eq!(
            state.region.rail_authority.finances.next_fiscal_period_at,
            Some(UtcSeconds::from_unix_seconds(4 * 86_400))
        );
    }

    #[test]
    fn fiscal_calendar_caps_long_offline_catch_up_at_three_periods() {
        let started = UtcSeconds::from_unix_seconds(10_000);
        let mut state = create_new_game(42, "One More Prime", started);
        let allocation = state
            .region
            .rail_authority
            .finances
            .regional_public_allocation;
        let treasury_before = state.region.rail_authority.finances.treasury;

        let now = UtcSeconds::from_unix_seconds(14 * 86_400 + 12 * 60 * 60);
        let processed = advance_authority_fiscal_periods(&mut state.region, now).unwrap();

        assert_eq!(processed, MAX_OFFLINE_FISCAL_CATCHUP_PERIODS);
        assert_eq!(
            state.region.rail_authority.finances.next_fiscal_period_at,
            Some(UtcSeconds::from_unix_seconds(15 * 86_400))
        );
        let maximum_without_maintenance = treasury_before
            .checked_add(
                allocation
                    .checked_mul(u64::from(MAX_OFFLINE_FISCAL_CATCHUP_PERIODS))
                    .unwrap(),
            )
            .unwrap();
        assert!(state.region.rail_authority.finances.treasury < maximum_without_maintenance);
    }

    #[test]
    fn starter_region_exposes_one_candidate_per_unconnected_settlement() {
        let region = generate_region(42);
        let candidates = evaluate_connection_candidates(&region, 42).unwrap();
        let connected = region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .map(|station| station.settlement_id)
            .collect::<HashSet<_>>();

        assert_eq!(candidates.len(), region.settlements.len() - connected.len());
        assert!(
            candidates
                .iter()
                .all(|candidate| !connected.contains(&candidate.settlement_id))
        );
    }

    #[test]
    fn candidate_evaluation_is_deterministic_and_ranked() {
        let region = generate_region(7);
        let first = evaluate_connection_candidates(&region, 7).unwrap();
        let second = evaluate_connection_candidates(&region, 7).unwrap();

        assert_eq!(first, second);
        assert!(first.windows(2).all(|pair| {
            pair[0].score.total > pair[1].score.total
                || (pair[0].score.total == pair[1].score.total
                    && pair[0].estimated_cost <= pair[1].estimated_cost)
        }));
    }

    #[test]
    fn every_candidate_connects_to_existing_network_with_positive_estimates() {
        let region = generate_region(99);
        let candidates = evaluate_connection_candidates(&region, 99).unwrap();

        for candidate in candidates {
            assert!(candidate.estimated_distance.metres() > 0);
            assert!(candidate.estimated_cost.cents() > 0);
            assert!(
                region
                    .rail_authority
                    .rail_network
                    .rail_stations
                    .iter()
                    .any(|station| station.id == candidate.connection_station_id)
            );
        }
    }

    #[test]
    fn local_rail_success_uses_the_best_bidirectional_market() {
        let region = generate_region(42);
        let mut demand = seed_directional_demand(&region, 42);
        let station_id = region.rail_authority.rail_network.rail_stations[0].id;
        let counterpart_id = region.rail_authority.rail_network.rail_stations[1].id;

        for pool in &mut demand {
            if pool.origin_station_id == station_id && pool.destination_station_id == counterpart_id
            {
                pool.market_maturity = MarketMaturity::from_basis_points(5_000).unwrap();
            } else if pool.origin_station_id == counterpart_id
                && pool.destination_station_id == station_id
            {
                pool.market_maturity = MarketMaturity::from_basis_points(3_000).unwrap();
            }
        }

        assert_eq!(local_rail_success_basis_points(&demand, station_id), 4_000);
    }

    #[test]
    fn deferred_reconsideration_requires_progressively_stronger_adoption() {
        assert_eq!(deferred_reconsideration_threshold(0), 5_500);
        assert_eq!(deferred_reconsideration_threshold(1), 7_000);
        assert_eq!(deferred_reconsideration_threshold(2), 8_500);
        assert_eq!(deferred_reconsideration_threshold(3), 10_000);
        assert_eq!(deferred_reconsideration_threshold(4), 10_001);
    }

    #[test]
    fn harder_construction_costs_more_for_the_same_distance() {
        let distance = DistanceMetres::new(40_000).unwrap();
        let low = estimated_connection_cost(distance, ConstructionDifficulty::Low).unwrap();
        let moderate =
            estimated_connection_cost(distance, ConstructionDifficulty::Moderate).unwrap();
        let high = estimated_connection_cost(distance, ConstructionDifficulty::High).unwrap();

        assert!(low < moderate);
        assert!(moderate < high);
    }

    #[test]
    fn fresh_network_does_not_immediately_create_a_connection_request() {
        let mut region = generate_region(42);
        let demand = seed_directional_demand(&region, 42);
        let now = UtcSeconds::from_unix_seconds(1_000);

        advance_infrastructure_planning(&mut region, 42, &demand, now).unwrap();

        assert!(region.rail_authority.infrastructure_projects.is_empty());
    }

    #[test]
    fn established_local_rail_market_creates_the_highest_ranked_eligible_request() {
        let mut region = generate_region(42);
        let demand = fully_mature_demand(&region, 42);
        let expected = evaluate_connection_candidates(&region, 42).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(1_000);

        advance_infrastructure_planning(&mut region, 42, &demand, now).unwrap();

        let [project] = region.rail_authority.infrastructure_projects.as_slice() else {
            panic!("expected exactly one planning project");
        };
        assert!(project.id.is_v4());
        assert_eq!(project.status, InfrastructureProjectStatus::Requested);
        assert_eq!(project.timeline.requested_at, now);
        let InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } = &project.kind
        else {
            panic!("connection candidate must become a New Line project");
        };
        assert_eq!(planned_stations.len(), 1);
        assert_eq!(planned_stations[0].settlement_id, expected.settlement_id);
        assert!(planned_stations[0].id.is_v4());
        assert_eq!(planned_lines.len(), 1);
        assert!(planned_lines[0].id.is_v4());
        assert_eq!(
            planned_lines[0].first_station_id,
            expected.connection_station_id
        );
        assert_eq!(planned_lines[0].second_station_id, planned_stations[0].id);
        assert_eq!(planned_lines[0].distance, expected.estimated_distance);
        assert_eq!(project.funding.estimated_cost, expected.estimated_cost);
        assert_eq!(project.funding.authority_committed, Money::ZERO);
        let [bulletin] = region.bulletin.as_slice() else {
            panic!("expected one persistent council Bulletin item");
        };
        assert_eq!(bulletin.category, BulletinCategory::Local);
        assert_eq!(bulletin.occurred_at, now);
        assert!(
            bulletin
                .headline
                .contains("Council requests railway connection")
        );
    }

    #[test]
    fn planning_advances_requested_project_to_approval_on_fixed_timeline() {
        let mut region = generate_region(7);
        let demand = fully_mature_demand(&region, 7);
        let started = UtcSeconds::from_unix_seconds(10_000);
        advance_infrastructure_planning(&mut region, 7, &demand, started).unwrap();
        let first_id = region.rail_authority.infrastructure_projects[0].id;
        let target_settlement_id = match &region.rail_authority.infrastructure_projects[0].kind {
            InfrastructureProjectKind::NewLine {
                planned_stations, ..
            } => planned_stations[0].settlement_id,
            _ => panic!("expected a New Line project"),
        };
        region
            .settlements
            .iter_mut()
            .find(|settlement| settlement.id == target_settlement_id)
            .unwrap()
            .population = 300_000;
        region.rail_authority.infrastructure_projects[0]
            .funding
            .estimated_cost = Money::ZERO;

        advance_infrastructure_planning(
            &mut region,
            7,
            &demand,
            UtcSeconds::from_unix_seconds(10_000 + 60 * 60),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::UnderReview
        );

        advance_infrastructure_planning(
            &mut region,
            7,
            &demand,
            UtcSeconds::from_unix_seconds(10_000 + 3 * 60 * 60),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Proposed
        );

        advance_infrastructure_planning(
            &mut region,
            7,
            &demand,
            UtcSeconds::from_unix_seconds(10_000 + 4 * 60 * 60),
        )
        .unwrap();

        let first = region
            .rail_authority
            .infrastructure_projects
            .iter()
            .find(|project| project.id == first_id)
            .unwrap();
        assert_eq!(first.status, InfrastructureProjectStatus::Approved);
        assert_eq!(
            first.timeline.approved_at,
            Some(UtcSeconds::from_unix_seconds(10_000 + 4 * 60 * 60))
        );
        // The council-request cooldown prevents a second request from appearing
        // immediately after the first review completes.
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 1);

        advance_infrastructure_planning(
            &mut region,
            7,
            &demand,
            UtcSeconds::from_unix_seconds(10_000 + 24 * 60 * 60),
        )
        .unwrap();
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 2);
        assert_eq!(
            region.rail_authority.infrastructure_projects[1].status,
            InfrastructureProjectStatus::Requested
        );
    }

    #[test]
    fn planning_defers_a_low_value_connection_after_review() {
        let mut region = generate_region(17);
        let mut demand = fully_mature_demand(&region, 17);
        let started = UtcSeconds::from_unix_seconds(30_000);
        advance_infrastructure_planning(&mut region, 17, &demand, started).unwrap();
        let first_id = region.rail_authority.infrastructure_projects[0].id;
        let target_settlement_id = match &region.rail_authority.infrastructure_projects[0].kind {
            InfrastructureProjectKind::NewLine {
                planned_stations, ..
            } => planned_stations[0].settlement_id,
            _ => panic!("expected a New Line project"),
        };

        region
            .settlements
            .iter_mut()
            .find(|settlement| settlement.id == target_settlement_id)
            .unwrap()
            .population = 0;
        region.rail_authority.infrastructure_projects[0]
            .funding
            .estimated_cost = Money::from_cents(25_000_000);

        advance_infrastructure_planning(
            &mut region,
            17,
            &demand,
            UtcSeconds::from_unix_seconds(30_000 + 4 * 60 * 60),
        )
        .unwrap();

        let first = region
            .rail_authority
            .infrastructure_projects
            .iter()
            .find(|project| project.id == first_id)
            .unwrap();
        assert_eq!(first.status, InfrastructureProjectStatus::Deferred);
        assert_eq!(
            first.timeline.deferred_at,
            Some(UtcSeconds::from_unix_seconds(30_000 + 4 * 60 * 60))
        );
        assert_eq!(first.timeline.approved_at, None);

        let duplicate_target_count = region
            .rail_authority
            .infrastructure_projects
            .iter()
            .filter(|project| match &project.kind {
                InfrastructureProjectKind::NewLine {
                    planned_stations, ..
                } => planned_stations
                    .iter()
                    .any(|station| station.settlement_id == target_settlement_id),
                _ => false,
            })
            .count();
        assert_eq!(duplicate_target_count, 1);

        // Deferred projects are not retried merely because time passed. The
        // first reconsideration requires renewed adoption pressure and a full day.
        for pool in &mut demand {
            pool.market_maturity = MarketMaturity::from_basis_points(3_900).unwrap();
        }
        advance_infrastructure_planning(
            &mut region,
            17,
            &demand,
            UtcSeconds::from_unix_seconds(30_000 + 28 * 60 * 60),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Deferred
        );

        for pool in &mut demand {
            pool.market_maturity = MarketMaturity::from_basis_points(5_500).unwrap();
        }
        advance_infrastructure_planning(
            &mut region,
            17,
            &demand,
            UtcSeconds::from_unix_seconds(30_000 + 28 * 60 * 60),
        )
        .unwrap();
        let reconsidered = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(reconsidered.id, first_id);
        assert_eq!(reconsidered.status, InfrastructureProjectStatus::Requested);
        assert_eq!(reconsidered.timeline.reconsideration_count, 1);
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 1);
    }

    #[test]
    fn planning_uses_configured_authority_cadence() {
        let mut region = generate_region(31);
        let demand = fully_mature_demand(&region, 31);
        let started = UtcSeconds::from_unix_seconds(50_000);
        let rules = AuthorityRules::new(
            DurationSeconds::from_seconds(10),
            DurationSeconds::from_seconds(20),
            DurationSeconds::from_seconds(30),
            DurationSeconds::from_seconds(1_000),
            DurationSeconds::from_seconds(1_000),
            DurationSeconds::from_seconds(60 * 60),
            DurationSeconds::from_seconds(5 * 60 * 60),
            2 * 60,
            3 * 60,
            4 * 60,
            2,
        );

        advance_infrastructure_planning_with_rules(&mut region, 31, &demand, &rules, started)
            .unwrap();
        let target_settlement_id = match &region.rail_authority.infrastructure_projects[0].kind {
            InfrastructureProjectKind::NewLine {
                planned_stations, ..
            } => planned_stations[0].settlement_id,
            _ => panic!("expected a New Line project"),
        };
        region
            .settlements
            .iter_mut()
            .find(|settlement| settlement.id == target_settlement_id)
            .unwrap()
            .population = 300_000;
        region.rail_authority.infrastructure_projects[0]
            .funding
            .estimated_cost = Money::ZERO;

        advance_infrastructure_planning_with_rules(
            &mut region,
            31,
            &demand,
            &rules,
            UtcSeconds::from_unix_seconds(50_009),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Requested
        );

        advance_infrastructure_planning_with_rules(
            &mut region,
            31,
            &demand,
            &rules,
            UtcSeconds::from_unix_seconds(50_060),
        )
        .unwrap();
        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Approved);
        assert_eq!(
            project.timeline.review_started_at,
            Some(UtcSeconds::from_unix_seconds(50_010))
        );
        assert_eq!(
            project.timeline.proposed_at,
            Some(UtcSeconds::from_unix_seconds(50_030))
        );
        assert_eq!(
            project.timeline.approved_at,
            Some(UtcSeconds::from_unix_seconds(50_060))
        );
    }

    #[test]
    fn expansion_pipeline_limit_blocks_new_requests_until_a_slot_opens() {
        let mut region = generate_region(37);
        let demand = fully_mature_demand(&region, 37);
        let candidates = evaluate_connection_candidates(&region, 37).unwrap();
        let requested_at = UtcSeconds::from_unix_seconds(1_000);
        let rules = AuthorityRules::provisional();

        region.rail_authority.infrastructure_projects = candidates
            .iter()
            .take(2)
            .cloned()
            .map(|candidate| {
                let mut project = project_from_candidate(candidate, requested_at);
                project.status = InfrastructureProjectStatus::Funding;
                project.timeline.approved_at = Some(requested_at);
                project
            })
            .collect();

        let now = UtcSeconds::from_unix_seconds(1_000 + 48 * 60 * 60);
        advance_infrastructure_planning_with_rules(&mut region, 37, &demand, &rules, now).unwrap();
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 2);

        region.rail_authority.infrastructure_projects[0].status = InfrastructureProjectStatus::Open;
        advance_infrastructure_planning_with_rules(&mut region, 37, &demand, &rules, now).unwrap();
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 3);
        assert_eq!(
            region.rail_authority.infrastructure_projects[2].status,
            InfrastructureProjectStatus::Requested
        );
    }

    #[test]
    fn offline_planning_catch_up_creates_at_most_one_fresh_request() {
        let mut region = generate_region(23);
        let demand = fully_mature_demand(&region, 23);
        let started = UtcSeconds::from_unix_seconds(40_000);
        advance_infrastructure_planning(&mut region, 23, &demand, started).unwrap();

        let target_settlement_id = match &region.rail_authority.infrastructure_projects[0].kind {
            InfrastructureProjectKind::NewLine {
                planned_stations, ..
            } => planned_stations[0].settlement_id,
            _ => panic!("expected a New Line project"),
        };
        region
            .settlements
            .iter_mut()
            .find(|settlement| settlement.id == target_settlement_id)
            .unwrap()
            .population = 300_000;
        region.rail_authority.infrastructure_projects[0]
            .funding
            .estimated_cost = Money::ZERO;

        let reopened = UtcSeconds::from_unix_seconds(40_000 + 48 * 60 * 60);
        advance_infrastructure_planning(&mut region, 23, &demand, reopened).unwrap();
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 2);
        assert_eq!(
            region.rail_authority.infrastructure_projects[1].status,
            InfrastructureProjectStatus::Requested
        );

        // Re-running reconciliation at the same timestamp is idempotent for
        // council request creation rather than draining the candidate list.
        advance_infrastructure_planning(&mut region, 23, &demand, reopened).unwrap();
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 2);
    }

    #[test]
    fn planning_does_not_request_the_same_settlement_twice() {
        let mut region = generate_region(99);
        let demand = fully_mature_demand(&region, 99);
        let started = 20_000;

        advance_infrastructure_planning(
            &mut region,
            99,
            &demand,
            UtcSeconds::from_unix_seconds(started),
        )
        .unwrap();
        advance_infrastructure_planning(
            &mut region,
            99,
            &demand,
            UtcSeconds::from_unix_seconds(started + 60 * 60),
        )
        .unwrap();

        let targets = region
            .rail_authority
            .infrastructure_projects
            .iter()
            .filter_map(|project| match &project.kind {
                InfrastructureProjectKind::NewLine {
                    planned_stations, ..
                } => planned_stations
                    .first()
                    .map(|station| station.settlement_id),
                _ => None,
            })
            .collect::<HashSet<_>>();

        assert_eq!(
            targets.len(),
            region.rail_authority.infrastructure_projects.len()
        );
    }
    #[test]
    fn funding_commits_available_authority_investment_to_approved_project() {
        let mut region = generate_region(42);
        let candidate = evaluate_connection_candidates(&region, 42).unwrap()[0].clone();
        let cost = candidate.estimated_cost;
        let now = UtcSeconds::from_unix_seconds(5_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Approved;
        project.timeline.approved_at = Some(now);
        region.rail_authority.infrastructure_projects = vec![project];
        region.rail_authority.finances.treasury =
            cost.checked_add(Money::from_cents(10_000)).unwrap();
        region.rail_authority.finances.maintenance_reserve = Money::ZERO;
        region.rail_authority.finances.committed_investment = Money::ZERO;

        advance_project_funding(&mut region, now).unwrap();

        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Funding);
        assert_eq!(project.funding.authority_committed, cost);
        assert_eq!(project.timeline.funding_completed_at, Some(now));
        assert_eq!(region.rail_authority.finances.committed_investment, cost);
    }

    #[test]
    fn funding_can_leave_project_partially_funded_when_budget_is_insufficient() {
        let mut region = generate_region(7);
        let candidate = evaluate_connection_candidates(&region, 7).unwrap()[0].clone();
        let cost = candidate.estimated_cost;
        let partial = Money::from_cents((cost.cents() / 2).max(1));
        let now = UtcSeconds::from_unix_seconds(8_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Approved;
        project.timeline.approved_at = Some(now);
        region.rail_authority.infrastructure_projects = vec![project];
        region.rail_authority.finances.treasury = partial;
        region.rail_authority.finances.maintenance_reserve = Money::ZERO;
        region.rail_authority.finances.committed_investment = Money::ZERO;

        advance_project_funding(&mut region, now).unwrap();

        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Funding);
        assert_eq!(project.funding.authority_committed, partial);
        assert_eq!(project.timeline.funding_completed_at, None);
        assert_eq!(region.rail_authority.finances.committed_investment, partial);
        assert!(project.funding.funding_gap().unwrap() > Money::ZERO);
    }

    #[test]
    fn funding_pipeline_keeps_excess_approved_projects_out_of_funding() {
        let mut region = generate_region(71);
        let candidates = evaluate_connection_candidates(&region, 71).unwrap();
        let now = UtcSeconds::from_unix_seconds(9_000);
        region.rail_authority.construction_capacity = 1;
        region.rail_authority.finances.treasury = Money::ZERO;
        region.rail_authority.finances.maintenance_reserve = Money::ZERO;

        region.rail_authority.infrastructure_projects = candidates
            .into_iter()
            .take(4)
            .enumerate()
            .map(|(offset, candidate)| {
                let requested = UtcSeconds::from_unix_seconds(9_000 + offset as i64);
                let mut project = project_from_candidate(candidate, requested);
                project.status = InfrastructureProjectStatus::Approved;
                project.timeline.approved_at = Some(requested);
                project
            })
            .collect();

        advance_project_funding(&mut region, now).unwrap();

        assert_eq!(region.rail_authority.funding_pipeline_capacity(), 2);
        assert_eq!(region.rail_authority.active_funding_count(), 2);
        assert_eq!(
            region
                .rail_authority
                .infrastructure_projects
                .iter()
                .filter(|project| project.status == InfrastructureProjectStatus::Approved)
                .count(),
            2
        );
    }

    #[test]
    fn funding_pipeline_demotes_only_untouched_excess_legacy_projects() {
        let mut region = generate_region(73);
        let candidates = evaluate_connection_candidates(&region, 73).unwrap();
        let now = UtcSeconds::from_unix_seconds(10_000);
        region.rail_authority.construction_capacity = 1;
        region.rail_authority.finances.treasury = Money::ZERO;
        region.rail_authority.finances.maintenance_reserve = Money::ZERO;

        let mut projects = candidates
            .into_iter()
            .take(4)
            .enumerate()
            .map(|(offset, candidate)| {
                let requested = UtcSeconds::from_unix_seconds(10_000 + offset as i64);
                let mut project = project_from_candidate(candidate, requested);
                project.status = InfrastructureProjectStatus::Funding;
                project.timeline.approved_at = Some(requested);
                project
            })
            .collect::<Vec<_>>();
        projects[2].funding.operator_contributed = Money::from_cents(100);
        region.rail_authority.infrastructure_projects = projects;

        advance_project_funding(&mut region, now).unwrap();

        assert_eq!(
            region.rail_authority.infrastructure_projects[2].status,
            InfrastructureProjectStatus::Funding
        );
        assert_eq!(
            region.rail_authority.infrastructure_projects[3].status,
            InfrastructureProjectStatus::Approved
        );
    }

    #[test]
    fn fully_funded_project_reserves_available_construction_capacity() {
        let mut region = generate_region(11);
        let candidate = evaluate_connection_candidates(&region, 11).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(12_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Funding;
        project.funding.authority_committed = project.funding.estimated_cost;
        project.timeline.funding_completed_at = Some(now);
        region.rail_authority.infrastructure_projects = vec![project];

        advance_project_scheduling(&mut region, now).unwrap();

        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Scheduled);
        assert_eq!(
            project.timeline.scheduled_start_at,
            Some(UtcSeconds::from_unix_seconds(12_000 + 60 * 60))
        );
        assert_eq!(region.rail_authority.reserved_construction_count(), 1);
        assert_eq!(region.rail_authority.construction_slots_remaining(), 0);
    }

    #[test]
    fn fully_funded_project_waits_when_capacity_is_already_reserved() {
        let mut region = generate_region(13);
        let candidates = evaluate_connection_candidates(&region, 13).unwrap();
        let now = UtcSeconds::from_unix_seconds(20_000);

        let mut first = project_from_candidate(candidates[0].clone(), now);
        first.status = InfrastructureProjectStatus::Funding;
        first.funding.authority_committed = first.funding.estimated_cost;
        first.timeline.funding_completed_at = Some(now);

        let later = UtcSeconds::from_unix_seconds(20_001);
        let mut second = project_from_candidate(candidates[1].clone(), later);
        second.status = InfrastructureProjectStatus::Funding;
        second.funding.authority_committed = second.funding.estimated_cost;
        second.timeline.funding_completed_at = Some(later);

        region.rail_authority.construction_capacity = 1;
        region.rail_authority.infrastructure_projects = vec![second, first];

        advance_project_scheduling(&mut region, now).unwrap();

        let scheduled = region
            .rail_authority
            .infrastructure_projects
            .iter()
            .filter(|project| project.status == InfrastructureProjectStatus::Scheduled)
            .collect::<Vec<_>>();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].timeline.funding_completed_at, Some(now));
        assert_eq!(
            region
                .rail_authority
                .infrastructure_projects
                .iter()
                .filter(|project| project.status == InfrastructureProjectStatus::Funding)
                .count(),
            1
        );
    }

    #[test]
    fn new_line_construction_duration_scales_with_distance_and_difficulty() {
        let region = generate_region(17);
        let candidate = evaluate_connection_candidates(&region, 17).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(30_000);
        let mut project = project_from_candidate(candidate, now);
        let InfrastructureProjectKind::NewLine { planned_lines, .. } = &mut project.kind else {
            panic!("connection candidate must become a New Line project");
        };
        planned_lines[0].distance = DistanceMetres::new(10_000).unwrap();
        planned_lines[0].construction_difficulty = ConstructionDifficulty::Moderate;

        assert_eq!(
            new_line_construction_duration(&project).unwrap(),
            DurationSeconds::from_seconds(5 * 60 * 60 + 10 * 3 * 60)
        );
    }

    #[test]
    fn construction_uses_configured_mobilisation_and_duration() {
        let mut region = generate_region(43);
        let candidate = evaluate_connection_candidates(&region, 43).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(35_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Funding;
        project.funding.authority_committed = project.funding.estimated_cost;
        project.timeline.funding_completed_at = Some(now);
        let InfrastructureProjectKind::NewLine { planned_lines, .. } = &mut project.kind else {
            panic!("connection candidate must become a New Line project");
        };
        planned_lines[0].distance = DistanceMetres::new(10_000).unwrap();
        planned_lines[0].construction_difficulty = ConstructionDifficulty::Moderate;
        region.rail_authority.infrastructure_projects = vec![project];

        let rules = AuthorityRules::new(
            DurationSeconds::from_seconds(10),
            DurationSeconds::from_seconds(20),
            DurationSeconds::from_seconds(30),
            DurationSeconds::from_seconds(40),
            DurationSeconds::from_seconds(50),
            DurationSeconds::from_seconds(37),
            DurationSeconds::from_seconds(100),
            1,
            2,
            3,
            2,
        );

        advance_project_scheduling_with_rules(&mut region, &rules, now).unwrap();
        let scheduled_start = UtcSeconds::from_unix_seconds(35_037);
        assert_eq!(
            region.rail_authority.infrastructure_projects[0]
                .timeline
                .scheduled_start_at,
            Some(scheduled_start)
        );
        assert_eq!(
            new_line_construction_duration_with_rules(
                &region.rail_authority.infrastructure_projects[0],
                &rules,
            )
            .unwrap(),
            DurationSeconds::from_seconds(120)
        );

        advance_project_construction_with_rules(&mut region, &rules, scheduled_start).unwrap();
        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Construction);
        assert_eq!(
            project.timeline.construction_started_at,
            Some(scheduled_start)
        );
        assert_eq!(
            project.timeline.planned_completion_at,
            Some(UtcSeconds::from_unix_seconds(35_157))
        );
    }

    #[test]
    fn scheduled_new_line_starts_at_fixed_timestamp_and_keeps_completion_date() {
        let mut region = generate_region(19);
        let candidate = evaluate_connection_candidates(&region, 19).unwrap()[0].clone();
        let scheduled_start = UtcSeconds::from_unix_seconds(40_000);
        let mut project = project_from_candidate(candidate, UtcSeconds::from_unix_seconds(30_000));
        project.status = InfrastructureProjectStatus::Scheduled;
        project.funding.authority_committed = project.funding.estimated_cost;
        project.timeline.funding_completed_at = Some(UtcSeconds::from_unix_seconds(39_000));
        project.timeline.scheduled_start_at = Some(scheduled_start);
        let duration = new_line_construction_duration(&project).unwrap();
        let expected_completion = scheduled_start.checked_add(duration).unwrap();
        region.rail_authority.infrastructure_projects = vec![project];

        advance_project_construction(
            &mut region,
            UtcSeconds::from_unix_seconds(scheduled_start.unix_seconds() - 1),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Scheduled
        );

        // Reconcile well after the scheduled timestamp to model reopening RailQ
        // after construction should already have started.
        advance_project_construction(
            &mut region,
            UtcSeconds::from_unix_seconds(scheduled_start.unix_seconds() + 3_600),
        )
        .unwrap();
        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Construction);
        assert_eq!(
            project.timeline.construction_started_at,
            Some(scheduled_start)
        );
        assert_eq!(
            project.timeline.planned_completion_at,
            Some(expected_completion)
        );

        advance_project_construction(
            &mut region,
            UtcSeconds::from_unix_seconds(scheduled_start.unix_seconds() + 7_200),
        )
        .unwrap();
        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(
            project.timeline.construction_started_at,
            Some(scheduled_start)
        );
        assert_eq!(
            project.timeline.planned_completion_at,
            Some(expected_completion)
        );
    }

    #[test]
    fn completed_new_line_materialises_network_and_spends_commitment_once() {
        let mut region = generate_region(41);
        let candidate = evaluate_connection_candidates(&region, 41).unwrap()[0].clone();
        let completion = UtcSeconds::from_unix_seconds(90_000);
        let mut project = project_from_candidate(candidate, UtcSeconds::from_unix_seconds(80_000));
        project.status = InfrastructureProjectStatus::Construction;
        project.timeline.construction_started_at = Some(UtcSeconds::from_unix_seconds(85_000));
        project.timeline.planned_completion_at = Some(completion);
        project.funding.authority_committed = project.funding.estimated_cost;
        let committed = project.funding.authority_committed;
        let (planned_station, planned_line) = match &project.kind {
            InfrastructureProjectKind::NewLine {
                planned_stations,
                planned_lines,
            } => (planned_stations[0].clone(), planned_lines[0].clone()),
            _ => panic!("connection candidate must become a New Line project"),
        };
        let station_count = region.rail_authority.rail_network.rail_stations.len();
        let line_count = region.rail_authority.rail_network.rail_lines.len();
        region.rail_authority.finances.treasury = committed
            .checked_add(Money::from_cents(50_000_000))
            .unwrap();
        region.rail_authority.finances.committed_investment = committed;
        region.rail_authority.infrastructure_projects = vec![project];
        let treasury_before = region.rail_authority.finances.treasury;

        open_completed_infrastructure_projects(
            &mut region,
            UtcSeconds::from_unix_seconds(completion.unix_seconds() - 1),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.rail_network.rail_stations.len(),
            station_count
        );
        assert_eq!(
            region.rail_authority.rail_network.rail_lines.len(),
            line_count
        );

        open_completed_infrastructure_projects(
            &mut region,
            UtcSeconds::from_unix_seconds(completion.unix_seconds() + 3_600),
        )
        .unwrap();

        let opened = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(opened.status, InfrastructureProjectStatus::Open);
        assert_eq!(opened.timeline.completed_at, Some(completion));
        assert_eq!(
            region.rail_authority.rail_network.rail_stations.len(),
            station_count + 1
        );
        assert_eq!(
            region.rail_authority.rail_network.rail_lines.len(),
            line_count + 1
        );
        assert!(
            region
                .rail_authority
                .rail_network
                .rail_stations
                .iter()
                .any(|station| station.id == planned_station.id
                    && station.settlement_id == planned_station.settlement_id)
        );
        assert!(
            region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .any(|line| line.id == planned_line.id
                    && line.first_station_id == planned_line.first_station_id
                    && line.second_station_id == planned_line.second_station_id
                    && line.distance == planned_line.distance
                    && line.speed_limit == planned_line.speed_limit
                    && line.track_count == planned_line.track_count
                    && line.electrification == planned_line.electrification)
        );
        assert_eq!(
            region.rail_authority.finances.treasury,
            treasury_before.checked_sub(committed).unwrap()
        );
        assert_eq!(
            region.rail_authority.finances.committed_investment,
            Money::ZERO
        );
        assert_eq!(
            region.rail_authority.finances.maintenance_reserve,
            region
                .rail_authority
                .rail_network
                .provisional_maintenance_reserve()
                .unwrap()
        );

        // Reconciliation remains idempotent after the project is Open.
        let treasury_after_open = region.rail_authority.finances.treasury;
        open_completed_infrastructure_projects(
            &mut region,
            UtcSeconds::from_unix_seconds(completion.unix_seconds() + 7_200),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.rail_network.rail_stations.len(),
            station_count + 1
        );
        assert_eq!(
            region.rail_authority.rail_network.rail_lines.len(),
            line_count + 1
        );
        assert_eq!(region.rail_authority.finances.treasury, treasury_after_open);
    }

    #[test]
    fn cancelling_funded_project_releases_authority_commitment() {
        let mut region = generate_region(23);
        let candidate = evaluate_connection_candidates(&region, 23).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(50_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Funding;
        let committed = project.funding.estimated_cost;
        project.funding.authority_committed = committed;
        let project_id = project.id;
        region.rail_authority.finances.committed_investment = committed;
        region.rail_authority.infrastructure_projects = vec![project];

        cancel_infrastructure_project(
            &mut region,
            project_id,
            UtcSeconds::from_unix_seconds(50_100),
        )
        .unwrap();

        let project = &region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.status, InfrastructureProjectStatus::Cancelled);
        assert_eq!(
            project.timeline.cancelled_at,
            Some(UtcSeconds::from_unix_seconds(50_100))
        );
        assert_eq!(
            region.rail_authority.finances.committed_investment,
            Money::ZERO
        );
        assert_eq!(project.funding.authority_committed, Money::ZERO);
    }

    #[test]
    fn cancelling_scheduled_project_releases_construction_capacity() {
        let mut region = generate_region(29);
        let candidate = evaluate_connection_candidates(&region, 29).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(60_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Scheduled;
        project.timeline.scheduled_start_at = Some(UtcSeconds::from_unix_seconds(61_000));
        project.funding.authority_committed = project.funding.estimated_cost;
        let committed = project.funding.authority_committed;
        let project_id = project.id;
        region.rail_authority.finances.committed_investment = committed;
        region.rail_authority.construction_capacity = 1;
        region.rail_authority.infrastructure_projects = vec![project];

        assert_eq!(region.rail_authority.construction_slots_remaining(), 0);
        cancel_infrastructure_project(&mut region, project_id, now).unwrap();
        assert_eq!(region.rail_authority.construction_slots_remaining(), 1);
    }

    #[test]
    fn construction_project_cannot_be_cancelled() {
        let mut region = generate_region(31);
        let candidate = evaluate_connection_candidates(&region, 31).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(70_000);
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Construction;
        let project_id = project.id;
        region.rail_authority.infrastructure_projects = vec![project];

        assert_eq!(
            cancel_infrastructure_project(&mut region, project_id, now),
            Err(InfrastructureProjectActionError::CannotCancel {
                project_id,
                status: InfrastructureProjectStatus::Construction,
            })
        );
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Construction
        );
    }

    #[test]
    fn cancelling_unknown_project_is_rejected() {
        let mut region = generate_region(37);
        let project_id = InfrastructureProjectId::new_v4();

        assert_eq!(
            cancel_infrastructure_project(
                &mut region,
                project_id,
                UtcSeconds::from_unix_seconds(80_000),
            ),
            Err(InfrastructureProjectActionError::ProjectNotFound { project_id })
        );
    }
    #[test]
    fn operator_contribution_reduces_company_cash_and_project_gap() {
        let now = UtcSeconds::from_unix_seconds(90_000);
        let mut game = create_new_game(41, "Operator", now);
        let candidate = evaluate_connection_candidates(&game.region, 41).unwrap()[0].clone();
        let mut project = project_from_candidate(candidate, now);
        project.status = InfrastructureProjectStatus::Funding;
        let project_id = project.id;
        game.player_company.funds = Money::from_cents(10_000_000);
        let amount = project
            .funding
            .suggested_operator_contribution(game.player_company.funds)
            .unwrap();
        let gap_before = project.funding.funding_gap().unwrap();
        let funds_before = game.player_company.funds;
        game.region.rail_authority.infrastructure_projects = vec![project];

        contribute_to_infrastructure_project(&mut game, project_id, amount, now).unwrap();

        let project = &game.region.rail_authority.infrastructure_projects[0];
        assert_eq!(project.funding.operator_contributed, amount);
        assert_eq!(
            game.player_company.funds,
            funds_before.checked_sub(amount).unwrap()
        );
        assert_eq!(
            project.funding.funding_gap().unwrap(),
            gap_before.checked_sub(amount).unwrap()
        );
        assert!(
            project.funding.operator_contributed
                <= project.funding.operator_contribution_cap().unwrap()
        );
    }
}

//! Rail Authority infrastructure planning helpers.
//!
//! Candidate evaluation and the early Rail Authority planning lifecycle.
//!
//! This layer currently advances projects through public funding, scheduling,
//! and fixed-duration construction. Opening is introduced later.

use std::{error::Error, fmt};

use crate::model::{
    CalculationError, ConstructionDifficulty, DistanceMetres, DurationSeconds, Electrification,
    GameState, InfrastructureProject, InfrastructureProjectFunding, InfrastructureProjectId,
    InfrastructureProjectKind, InfrastructureProjectStatus, InfrastructureProjectTimeline, Money,
    MoneyPerKilometre, PlannedRailLine, PlannedRailStation, RailLine, RailLineId, RailStation,
    RailStationId, Region, SettlementId, SpeedKilometresPerHour,
    TrackCount, UtcSeconds,
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

// Provisional real-time planning cadence. These values are intentionally kept
// local to the Authority simulation until the first progression playtest.
const REQUEST_QUEUE_DELAY: DurationSeconds = DurationSeconds::from_seconds(15 * 60);
const REVIEW_DURATION: DurationSeconds = DurationSeconds::from_seconds(30 * 60);
const PROPOSAL_DURATION: DurationSeconds = DurationSeconds::from_seconds(15 * 60);
// A small mobilisation window keeps Scheduled visible as a real lifecycle
// state while reserving scarce construction capacity before work begins.
const CONSTRUCTION_MOBILISATION_DELAY: DurationSeconds = DurationSeconds::from_seconds(15 * 60);
// Provisional compressed construction cadence for the first progression playtest.
// Physical work remains deterministic once construction starts; later balancing
// may change these rates without changing the lifecycle model.
const NEW_LINE_BASE_CONSTRUCTION_DURATION: DurationSeconds =
    DurationSeconds::from_seconds(2 * 60 * 60);
const LOW_DIFFICULTY_SECONDS_PER_KILOMETRE: u64 = 2 * 60;
const MODERATE_DIFFICULTY_SECONDS_PER_KILOMETRE: u64 = 3 * 60;
const HIGH_DIFFICULTY_SECONDS_PER_KILOMETRE: u64 = 4 * 60;
/// Maximum number of missed daily fiscal periods applied when RailQ catches up
/// after being closed. Older missed periods are skipped so long absences do not
/// turn into unlimited unattended public funding.
const MAX_OFFLINE_FISCAL_CATCHUP_PERIODS: u32 = 3;

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
            Self::InvalidContribution => write!(formatter, "Infrastructure contribution must be positive"),
            Self::ContributionExceedsFundingGap => {
                write!(formatter, "Infrastructure contribution exceeds the remaining funding gap")
            }
            Self::ContributionExceedsOperatorCap => {
                write!(formatter, "Infrastructure contribution exceeds the operator contribution cap")
            }
            Self::InsufficientCompanyFunds => {
                write!(formatter, "Company Funds are too low for this infrastructure contribution")
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
    let authority = &mut region.rail_authority;
    let index = authority
        .infrastructure_projects
        .iter()
        .position(|project| project.id == project_id)
        .ok_or(InfrastructureProjectActionError::ProjectNotFound { project_id })?;

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
    project.funding.operator_contributed = project
        .funding
        .operator_contributed
        .checked_add(amount)?;
    if project.funding.is_fully_funded() && project.timeline.funding_completed_at.is_none() {
        project.timeline.funding_completed_at = Some(now);
    }
    state.player_company.funds = funds_after;
    Ok(())
}


/// Advances the Rail Authority's daily UTC fiscal calendar.
///
/// At each midnight the previous maintenance reserve is paid, remaining
/// uncommitted investment carries forward, the recurring regional public
/// allocation is deposited, and maintenance is reserved for the new day.
/// Multiple missed calendar days are processed when RailQ is reopened.
pub fn advance_authority_fiscal_periods(
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
        authority.finances.treasury = authority
            .finances
            .treasury
            .checked_sub(maintenance_paid)?;
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
/// Proposed at a time. Approved projects may accumulate and later compete for
/// the Authority's finite investment budget.
pub fn advance_infrastructure_planning(
    region: &mut Region,
    world_seed: u64,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    advance_existing_planning_projects(region, now)?;

    if region
        .rail_authority
        .infrastructure_projects
        .iter()
        .any(|project| is_active_planning_status(project.status))
    {
        return Ok(());
    }

    let candidates = evaluate_connection_candidates(region, world_seed)?;
    let next_candidate = candidates.into_iter().find(|candidate| {
        !region
            .rail_authority
            .infrastructure_projects
            .iter()
            .any(|project| project_targets_settlement(project, candidate.settlement_id))
    });

    if let Some(candidate) = next_candidate {
        region
            .rail_authority
            .infrastructure_projects
            .push(project_from_candidate(candidate, now));
    }

    Ok(())
}

/// Moves approved projects into the active Funding pipeline and commits
/// currently available Authority investment funds in approval order. Partial
/// funding is allowed, but only a small number of projects are actively funded
/// at once; later approved projects remain Approved until a funding slot opens.
pub fn advance_project_funding(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
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

    Ok(())
}

/// Reserves available construction capacity for fully funded projects in
/// funding-completion order. Projects that cannot reserve a slot remain in
/// `Funding`, even when their financial gap is already zero.
pub fn advance_project_scheduling(
    region: &mut Region,
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

        let scheduled_start = now.checked_add(CONSTRUCTION_MOBILISATION_DELAY)?;
        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.status = InfrastructureProjectStatus::Scheduled;
        project.timeline.scheduled_start_at = Some(scheduled_start);
    }

    Ok(())
}

/// Starts due New Line projects and locks in their physical completion time.
///
/// Reconciliation uses the scheduled start timestamp rather than `now`, so a
/// project that became due while RailQ was closed keeps the same construction
/// duration it would have had while the game was open. Once written, the
/// planned completion timestamp is never recomputed by funding changes.
pub fn advance_project_construction(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    for project in &mut region.rail_authority.infrastructure_projects {
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

        let duration = new_line_construction_duration(project)?;
        let planned_completion = scheduled_start.checked_add(duration)?;
        project.status = InfrastructureProjectStatus::Construction;
        project.timeline.construction_started_at = Some(scheduled_start);
        project.timeline.planned_completion_at = Some(planned_completion);
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
pub fn open_completed_infrastructure_projects(
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

        let project = &mut region.rail_authority.infrastructure_projects[index];
        project.funding.award_operator_access_credit()?;
        project.status = InfrastructureProjectStatus::Open;
        project.timeline.completed_at = Some(completion);
    }

    Ok(())
}

fn new_line_construction_duration(
    project: &InfrastructureProject,
) -> Result<DurationSeconds, CalculationError> {
    let InfrastructureProjectKind::NewLine { planned_lines, .. } = &project.kind else {
        return Ok(DurationSeconds::from_seconds(0));
    };

    let mut seconds = NEW_LINE_BASE_CONSTRUCTION_DURATION.seconds();
    for line in planned_lines {
        let seconds_per_kilometre = match line.construction_difficulty {
            ConstructionDifficulty::Low => LOW_DIFFICULTY_SECONDS_PER_KILOMETRE,
            ConstructionDifficulty::Moderate => MODERATE_DIFFICULTY_SECONDS_PER_KILOMETRE,
            ConstructionDifficulty::High => HIGH_DIFFICULTY_SECONDS_PER_KILOMETRE,
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
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    for project in &mut region.rail_authority.infrastructure_projects {
        loop {
            match project.status {
                InfrastructureProjectStatus::Requested => {
                    let due = project
                        .timeline
                        .requested_at
                        .checked_add(REQUEST_QUEUE_DELAY)?;
                    if due > now {
                        break;
                    }
                    project.status = InfrastructureProjectStatus::UnderReview;
                    project.timeline.review_started_at = Some(due);
                }
                InfrastructureProjectStatus::UnderReview => {
                    let started_at = project
                        .timeline
                        .review_started_at
                        .unwrap_or(project.timeline.requested_at);
                    let due = started_at.checked_add(REVIEW_DURATION)?;
                    if due > now {
                        break;
                    }
                    project.timeline.review_started_at.get_or_insert(started_at);
                    project.status = InfrastructureProjectStatus::Proposed;
                    project.timeline.proposed_at = Some(due);
                }
                InfrastructureProjectStatus::Proposed => {
                    let proposed_at = project
                        .timeline
                        .proposed_at
                        .or(project.timeline.review_started_at)
                        .unwrap_or(project.timeline.requested_at);
                    let due = proposed_at.checked_add(PROPOSAL_DURATION)?;
                    if due > now {
                        break;
                    }
                    project.timeline.proposed_at.get_or_insert(proposed_at);
                    project.status = InfrastructureProjectStatus::Approved;
                    project.timeline.approved_at = Some(due);
                }
                _ => break,
            }
        }
    }
    Ok(())
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
    if matches!(
        project.status,
        InfrastructureProjectStatus::Cancelled | InfrastructureProjectStatus::Deferred
    ) {
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
            ConstructionDifficulty, DistanceMetres, DurationSeconds, InfrastructureProjectId,
            InfrastructureProjectKind, InfrastructureProjectStatus, Money, UtcSeconds,
        },
        sim::world::{create_new_game, generate_region},
    };

    use super::{
        InfrastructureProjectActionError, advance_authority_fiscal_periods,
        advance_infrastructure_planning, advance_project_construction, advance_project_funding,
        advance_project_scheduling,
        cancel_infrastructure_project, contribute_to_infrastructure_project,
        estimated_connection_cost, evaluate_connection_candidates, new_line_construction_duration,
        open_completed_infrastructure_projects, project_from_candidate,
    };

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
            Some(UtcSeconds::from_unix_seconds(next.unix_seconds() + 24 * 60 * 60))
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
    fn planning_creates_the_highest_ranked_connection_request() {
        let mut region = generate_region(42);
        let expected = evaluate_connection_candidates(&region, 42).unwrap()[0].clone();
        let now = UtcSeconds::from_unix_seconds(1_000);

        advance_infrastructure_planning(&mut region, 42, now).unwrap();

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
    }

    #[test]
    fn planning_advances_requested_project_to_approval_on_fixed_timeline() {
        let mut region = generate_region(7);
        let started = UtcSeconds::from_unix_seconds(10_000);
        advance_infrastructure_planning(&mut region, 7, started).unwrap();
        let first_id = region.rail_authority.infrastructure_projects[0].id;

        advance_infrastructure_planning(
            &mut region,
            7,
            UtcSeconds::from_unix_seconds(10_000 + 15 * 60),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::UnderReview
        );

        advance_infrastructure_planning(
            &mut region,
            7,
            UtcSeconds::from_unix_seconds(10_000 + 45 * 60),
        )
        .unwrap();
        assert_eq!(
            region.rail_authority.infrastructure_projects[0].status,
            InfrastructureProjectStatus::Proposed
        );

        advance_infrastructure_planning(
            &mut region,
            7,
            UtcSeconds::from_unix_seconds(10_000 + 60 * 60),
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
            Some(UtcSeconds::from_unix_seconds(10_000 + 60 * 60))
        );
        assert_eq!(region.rail_authority.infrastructure_projects.len(), 2);
        assert_eq!(
            region.rail_authority.infrastructure_projects[1].status,
            InfrastructureProjectStatus::Requested
        );
    }

    #[test]
    fn planning_does_not_request_the_same_settlement_twice() {
        let mut region = generate_region(99);
        let started = 20_000;

        advance_infrastructure_planning(&mut region, 99, UtcSeconds::from_unix_seconds(started))
            .unwrap();
        advance_infrastructure_planning(
            &mut region,
            99,
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
            Some(UtcSeconds::from_unix_seconds(12_000 + 15 * 60))
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
            DurationSeconds::from_seconds(2 * 60 * 60 + 10 * 3 * 60)
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

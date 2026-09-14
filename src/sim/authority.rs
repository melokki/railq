//! Rail Authority infrastructure planning helpers.
//!
//! Candidate evaluation and the early Rail Authority planning lifecycle.
//!
//! This layer currently advances projects through public funding. Scheduling,
//! construction, and opening are introduced by later batches.

use crate::model::{
    CalculationError, ConstructionDifficulty, DistanceMetres, DurationSeconds, Electrification,
    InfrastructureProject, InfrastructureProjectFunding, InfrastructureProjectId,
    InfrastructureProjectKind,
    InfrastructureProjectStatus, InfrastructureProjectTimeline, Money, MoneyPerKilometre,
    PlannedRailLine, PlannedRailStation, RailLineId, RailStationId, Region, SettlementId,
    SpeedKilometresPerHour, TrackCount, UtcSeconds,
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

const MIN_ESTIMATED_DISTANCE_METRES: u64 = 15_000;
const ESTIMATED_DISTANCE_SPREAD_METRES: u64 = 70_001;

// Provisional real-time planning cadence. These values are intentionally kept
// local to the Authority simulation until the first progression playtest.
const REQUEST_QUEUE_DELAY: DurationSeconds = DurationSeconds::from_seconds(15 * 60);
const REVIEW_DURATION: DurationSeconds = DurationSeconds::from_seconds(30 * 60);
const PROPOSAL_DURATION: DurationSeconds = DurationSeconds::from_seconds(15 * 60);

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

/// Moves approved projects into Funding and commits currently available
/// Authority investment funds in approval order. Partial funding is allowed;
/// a project remains in Funding until later budget income closes the gap.
pub fn advance_project_funding(
    region: &mut Region,
    now: UtcSeconds,
) -> Result<(), CalculationError> {
    let authority = &mut region.rail_authority;
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
        if authority.infrastructure_projects[index].status
            == InfrastructureProjectStatus::Approved
        {
            authority.infrastructure_projects[index].status = InfrastructureProjectStatus::Funding;
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
        let gap = authority.infrastructure_projects[index].funding.funding_gap()?;
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
                        estimated_connection_distance(world_seed, settlement.id, station.id),
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

fn estimated_connection_distance(
    world_seed: u64,
    settlement_id: SettlementId,
    station_id: RailStationId,
) -> DistanceMetres {
    let variation = mix_seed(
        world_seed ^ settlement_id.get().rotate_left(17) ^ station_id.get().rotate_left(39),
    ) % ESTIMATED_DISTANCE_SPREAD_METRES;
    let metres = MIN_ESTIMATED_DISTANCE_METRES + variation;
    DistanceMetres::new(i64::try_from(metres).expect("provisional distance fits i64"))
        .expect("provisional connection distance is positive")
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
            ConstructionDifficulty, DistanceMetres, InfrastructureProjectKind,
            InfrastructureProjectStatus, Money, UtcSeconds,
        },
        sim::world::generate_region,
    };

    use super::{
        advance_infrastructure_planning, advance_project_funding, estimated_connection_cost,
        evaluate_connection_candidates,
    };

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

}

//! Rail Authority infrastructure planning helpers.
//!
//! This module deliberately starts with transient candidate evaluation. It does
//! not create or advance persisted infrastructure projects yet; later batches
//! consume these ranked candidates when the Authority begins reviewing and
//! approving work.

use crate::model::{
    CalculationError, ConstructionDifficulty, DistanceMetres, Money, MoneyPerKilometre,
    RailStationId, Region, SettlementId,
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
                        estimated_connection_distance(
                            world_seed,
                            settlement.id,
                            station.id,
                        ),
                    )
                })
                .min_by_key(|(station_id, distance)| (*distance, *station_id))
                .expect("non-empty Rail Network has at least one Station");

            let construction_difficulty = estimated_construction_difficulty(
                world_seed,
                settlement.id,
                connection_station_id,
            );
            let estimated_cost = estimated_connection_cost(
                estimated_distance,
                construction_difficulty,
            )?;
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
        world_seed
            ^ settlement_id.get().rotate_left(17)
            ^ station_id.get().rotate_left(39),
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

    let connected_settlement_count = region
        .rail_authority
        .rail_network
        .rail_stations
        .len() as u32;
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
            mix_seed(world_seed ^ settlement_id.get().rotate_left(7) ^ 0x5245_4749_4f4e_414c)
                % 121,
        )
        .expect("regional-development score fits u32");

    // €/$1,000 of provisional project cost costs one score point, capped so
    // expensive but strategically valuable connections remain possible.
    let non_negative_cost_cents = estimated_cost.cents().max(0) as u64;
    let construction_cost_penalty = u32::try_from((non_negative_cost_cents / 100_000).min(250))
        .unwrap_or(250);

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
        model::{ConstructionDifficulty, DistanceMetres},
        sim::world::generate_region,
    };

    use super::{estimated_connection_cost, evaluate_connection_candidates};

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
            assert!(region
                .rail_authority
                .rail_network
                .rail_stations
                .iter()
                .any(|station| station.id == candidate.connection_station_id));
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
}

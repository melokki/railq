//! Elapsed-time replenishment for directional origin-destination demand.

use std::collections::{HashMap, HashSet};

use crate::{
    model::{
        GameState, InfrastructureProjectKind, InfrastructureProjectStatus, MarketMaturity,
        OriginDestinationDemand, PassengerArrivalRate, RailStationId, Region, UtcSeconds,
    },
    sim::services::path_between_stations,
};

const SECONDS_PER_HOUR: u128 = 60 * 60;
const INITIAL_DEMAND_HOURS: u32 = 3;
const INITIAL_MARKET_MATURITY_BASIS_POINTS: i64 = 2_500;

// Newly opened rail markets need time to establish themselves. The mature OD
// rate remains the single source of truth; this provisional ramp only scales
// how quickly passengers appear during the first six hours after opening.
const NEW_MARKET_STAGE_SECONDS: i64 = 2 * 60 * 60;
const NEW_MARKET_MATURITY_SECONDS: i64 = 3 * NEW_MARKET_STAGE_SECONDS;

// A 70 km/h railway is the current starter-network reference point. Better
// infrastructure can make rail more attractive, but the first implementation
// deliberately keeps the effect modest until services/frequency are modelled.
const REFERENCE_LINE_SPEED_KMH: u64 = 70;
const MIN_INFRASTRUCTURE_ATTRACTIVENESS_PERCENT: u32 = 75;
const MAX_INFRASTRUCTURE_ATTRACTIVENESS_PERCENT: u32 = 125;

/// Seeds one directional demand pool for every ordered pair of connected Rail
/// Stations. Unconnected Settlements have no Rail Station and therefore no
/// pool.
pub fn seed_directional_demand(region: &Region, world_seed: u64) -> Vec<OriginDestinationDemand> {
    let station_ids = region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect::<Vec<_>>();

    station_ids
        .iter()
        .copied()
        .flat_map(|origin_station_id| {
            station_ids
                .iter()
                .copied()
                .filter(move |&destination_station_id| destination_station_id != origin_station_id)
                .map(move |destination_station_id| {
                    let passenger_arrival_rate_per_hour =
                        seeded_arrival_rate(world_seed, origin_station_id, destination_station_id);
                    OriginDestinationDemand {
                        origin_station_id,
                        destination_station_id,
                        waiting_passengers: passenger_arrival_rate_per_hour
                            .passengers_per_hour()
                            .saturating_mul(INITIAL_DEMAND_HOURS),
                        market_maturity: MarketMaturity::from_basis_points(
                            INITIAL_MARKET_MATURITY_BASIS_POINTS,
                        )
                        .expect("initial market maturity is valid"),
                        passenger_arrival_rate_per_hour,
                        fractional_passenger_seconds: 0,
                    }
                })
        })
        .collect()
}

/// Advances every directional pool to `now`.
///
/// The effective time is never earlier than the previous update, which makes
/// equal or backward timestamps idempotent. Accumulation uses one arithmetic
/// operation per pool, even for long offline gaps.
pub fn replenish_directional_demand(state: &mut GameState, now: UtcSeconds) {
    let effective_now = now.max(state.last_processed_at);
    let interval_start = state.last_processed_at;
    let cap_duration_seconds = state.rules.demand.cap_duration.seconds();
    let station_opened_at = station_opening_times(&state.region);
    let effective_rates = state
        .origin_destination_demand
        .iter()
        .map(|pool| effective_arrival_rate_per_hour(state, pool))
        .collect::<Vec<_>>();

    for (pool, effective_rate_per_hour) in state
        .origin_destination_demand
        .iter_mut()
        .zip(effective_rates)
    {
        let market_opened_at = market_opening_time(
            &station_opened_at,
            pool.origin_station_id,
            pool.destination_station_id,
        );
        let effective_elapsed_seconds =
            market_effective_elapsed_seconds(interval_start, effective_now, market_opened_at);
        replenish_pool(
            pool,
            effective_elapsed_seconds,
            cap_duration_seconds,
            effective_rate_per_hour,
        );
    }
    state.last_processed_at = effective_now;
}

/// Creates any directional OD pools made possible by newly opened Stations.
///
/// This is intentionally idempotent. When RailQ is reopened after a project
/// completed while offline, a newly created market catches up only from the
/// infrastructure opening timestamp and uses the same maturity ramp as normal
/// demand replenishment. Existing markets are never reseeded or overwritten.
pub fn synchronize_directional_demand_with_network(state: &mut GameState, now: UtcSeconds) {
    let station_ids = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect::<Vec<_>>();
    let station_opened_at = station_opening_times(&state.region);
    let mut existing_pairs = state
        .origin_destination_demand
        .iter()
        .map(|pool| (pool.origin_station_id, pool.destination_station_id))
        .collect::<HashSet<_>>();
    let cap_duration_seconds = state.rules.demand.cap_duration.seconds();

    for origin_station_id in station_ids.iter().copied() {
        for destination_station_id in station_ids.iter().copied() {
            if origin_station_id == destination_station_id
                || !existing_pairs.insert((origin_station_id, destination_station_id))
            {
                continue;
            }

            let passenger_arrival_rate_per_hour =
                seeded_arrival_rate(state.world_seed, origin_station_id, destination_station_id);
            let mut pool = OriginDestinationDemand {
                origin_station_id,
                destination_station_id,
                waiting_passengers: 0,
                market_maturity: MarketMaturity::from_basis_points(
                    INITIAL_MARKET_MATURITY_BASIS_POINTS,
                )
                .expect("initial market maturity is valid"),
                passenger_arrival_rate_per_hour,
                fractional_passenger_seconds: 0,
            };

            if let Some(opened_at) = market_opening_time(
                &station_opened_at,
                origin_station_id,
                destination_station_id,
            ) {
                let effective_elapsed_seconds =
                    market_effective_elapsed_seconds(opened_at, now, Some(opened_at));
                let effective_rate_per_hour = effective_arrival_rate_for_pair(
                    state,
                    origin_station_id,
                    destination_station_id,
                    passenger_arrival_rate_per_hour.passengers_per_hour(),
                );
                replenish_pool(
                    &mut pool,
                    effective_elapsed_seconds,
                    cap_duration_seconds,
                    effective_rate_per_hour,
                );
            }

            state.origin_destination_demand.push(pool);
        }
    }
}

fn station_opening_times(region: &Region) -> HashMap<RailStationId, UtcSeconds> {
    let mut opened_at = HashMap::new();

    for project in &region.rail_authority.infrastructure_projects {
        if project.status != InfrastructureProjectStatus::Open {
            continue;
        }
        let Some(completed_at) = project.timeline.completed_at else {
            continue;
        };
        let InfrastructureProjectKind::NewLine {
            planned_stations, ..
        } = &project.kind
        else {
            continue;
        };

        for station in planned_stations {
            opened_at
                .entry(station.id)
                .and_modify(|current: &mut UtcSeconds| *current = (*current).min(completed_at))
                .or_insert(completed_at);
        }
    }

    opened_at
}

fn market_opening_time(
    station_opened_at: &HashMap<RailStationId, UtcSeconds>,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> Option<UtcSeconds> {
    match (
        station_opened_at.get(&origin_station_id).copied(),
        station_opened_at.get(&destination_station_id).copied(),
    ) {
        (Some(origin), Some(destination)) => Some(origin.max(destination)),
        (Some(origin), None) => Some(origin),
        (None, Some(destination)) => Some(destination),
        (None, None) => None,
    }
}

/// Converts wall-clock seconds into mature-market-equivalent seconds.
///
/// New markets run at 25%, 50%, and 75% of their mature arrival rate for two
/// hours each, then at 100%. Mature starter markets simply return the elapsed
/// wall-clock duration.
fn market_effective_elapsed_seconds(
    interval_start: UtcSeconds,
    interval_end: UtcSeconds,
    market_opened_at: Option<UtcSeconds>,
) -> u128 {
    let end = interval_end.unix_seconds();
    let start = interval_start.unix_seconds().min(end);
    let Some(opened_at) = market_opened_at.map(UtcSeconds::unix_seconds) else {
        return u128::try_from(end.saturating_sub(start)).unwrap_or(0);
    };

    let start = start.max(opened_at);
    if end <= start {
        return 0;
    }

    let boundaries = [
        opened_at,
        opened_at.saturating_add(NEW_MARKET_STAGE_SECONDS),
        opened_at.saturating_add(2 * NEW_MARKET_STAGE_SECONDS),
        opened_at.saturating_add(NEW_MARKET_MATURITY_SECONDS),
    ];
    let mut weighted_seconds = 0_u128;

    for (stage_start, stage_end, percentage) in [
        (boundaries[0], boundaries[1], 25_u128),
        (boundaries[1], boundaries[2], 50_u128),
        (boundaries[2], boundaries[3], 75_u128),
    ] {
        let overlap_start = start.max(stage_start);
        let overlap_end = end.min(stage_end);
        if overlap_end > overlap_start {
            weighted_seconds = weighted_seconds.saturating_add(
                u128::try_from(overlap_end - overlap_start)
                    .unwrap_or(0)
                    .saturating_mul(percentage)
                    / 100,
            );
        }
    }

    let mature_start = start.max(boundaries[3]);
    if end > mature_start {
        weighted_seconds =
            weighted_seconds.saturating_add(u128::try_from(end - mature_start).unwrap_or(0));
    }

    weighted_seconds
}

/// Returns the currently effective hourly Passenger Demand for one OD market.
///
/// `passenger_arrival_rate_per_hour` remains the market's seeded/base rate. The
/// effective rate is derived from the infrastructure path so future speed
/// upgrades can influence demand without permanently rewriting that base rate.
pub fn effective_arrival_rate_per_hour(
    state: &GameState,
    pool: &OriginDestinationDemand,
) -> u32 {
    effective_arrival_rate_for_pair(
        state,
        pool.origin_station_id,
        pool.destination_station_id,
        pool.passenger_arrival_rate_per_hour.passengers_per_hour(),
    )
}

/// Returns the waiting-passenger cap for the market at its current effective
/// infrastructure attractiveness.
pub fn waiting_passenger_cap(state: &GameState, pool: &OriginDestinationDemand) -> u32 {
    let rate = u128::from(effective_arrival_rate_per_hour(state, pool));
    (rate * u128::from(state.rules.demand.cap_duration.seconds()) / SECONDS_PER_HOUR)
        .min(u128::from(u32::MAX)) as u32
}

fn effective_arrival_rate_for_pair(
    state: &GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
    base_rate_per_hour: u32,
) -> u32 {
    let attractiveness = infrastructure_attractiveness_percent(
        &state.region,
        origin_station_id,
        destination_station_id,
    );
    let adjusted = u64::from(base_rate_per_hour)
        .saturating_mul(u64::from(attractiveness))
        .saturating_add(50)
        / 100;
    u32::try_from(adjusted.max(1)).unwrap_or(u32::MAX)
}

/// Provisional demand attractiveness from infrastructure journey quality.
///
/// The reference is the same physical path operated at 70 km/h throughout.
/// RailQ rewards half of the percentage journey-time saving, capped at +25%;
/// slower infrastructure can reduce the rate by at most 25%. This intentionally
/// leaves room for later service frequency, fare, comfort, and reliability
/// factors rather than letting line speed dominate Passenger Demand.
fn infrastructure_attractiveness_percent(
    region: &Region,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> u32 {
    let network = &region.rail_authority.rail_network;
    let Ok(path) = path_between_stations(network, origin_station_id, destination_station_id) else {
        return 100;
    };

    let mut actual_seconds = 0_u128;
    let mut reference_seconds = 0_u128;
    for rail_line_id in path {
        let Some(line) = network.rail_lines.iter().find(|line| line.id == rail_line_id) else {
            return 100;
        };
        actual_seconds = actual_seconds.saturating_add(duration_at_kmh(
            line.distance.metres(),
            u64::from(line.speed_limit.kilometres_per_hour()),
        ));
        reference_seconds = reference_seconds.saturating_add(duration_at_kmh(
            line.distance.metres(),
            REFERENCE_LINE_SPEED_KMH,
        ));
    }

    if reference_seconds == 0 || actual_seconds == 0 {
        return 100;
    }

    if actual_seconds <= reference_seconds {
        let saving_percent = reference_seconds
            .saturating_sub(actual_seconds)
            .saturating_mul(100)
            / reference_seconds;
        let uplift = u32::try_from(saving_percent / 2).unwrap_or(u32::MAX);
        100_u32
            .saturating_add(uplift)
            .min(MAX_INFRASTRUCTURE_ATTRACTIVENESS_PERCENT)
    } else {
        let extra_percent = actual_seconds
            .saturating_sub(reference_seconds)
            .saturating_mul(100)
            / reference_seconds;
        let penalty = u32::try_from(extra_percent / 2).unwrap_or(u32::MAX);
        100_u32
            .saturating_sub(penalty)
            .max(MIN_INFRASTRUCTURE_ATTRACTIVENESS_PERCENT)
    }
}

fn duration_at_kmh(distance_metres: u64, speed_kmh: u64) -> u128 {
    let numerator = u128::from(distance_metres).saturating_mul(3_600);
    let denominator = u128::from(speed_kmh).saturating_mul(1_000);
    numerator
        .saturating_add(denominator.saturating_sub(1))
        / denominator
}

fn replenish_pool(
    pool: &mut OriginDestinationDemand,
    elapsed_seconds: u128,
    cap_duration_seconds: u64,
    effective_rate_per_hour: u32,
) {
    let rate = u128::from(effective_rate_per_hour);
    let cap =
        (rate * u128::from(cap_duration_seconds) / SECONDS_PER_HOUR).min(u128::from(u32::MAX));
    let accumulated_passenger_seconds = rate
        .saturating_mul(elapsed_seconds)
        .saturating_add(u128::from(pool.fractional_passenger_seconds));
    let newly_waiting = accumulated_passenger_seconds / SECONDS_PER_HOUR;
    let remainder = accumulated_passenger_seconds % SECONDS_PER_HOUR;
    let waiting = u128::from(pool.waiting_passengers).saturating_add(newly_waiting);

    pool.waiting_passengers = waiting.min(cap) as u32;
    pool.fractional_passenger_seconds = if waiting >= cap { 0 } else { remainder as u64 };
}

fn seeded_arrival_rate(
    world_seed: u64,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> PassengerArrivalRate {
    let low_station_id = origin_station_id.get().min(destination_station_id.get());
    let high_station_id = origin_station_id.get().max(destination_station_id.get());
    let pair_variation =
        mix_seed(world_seed ^ low_station_id.rotate_left(17) ^ high_station_id) % 7;
    let direction_offset = if origin_station_id < destination_station_id {
        5
    } else {
        12
    };
    PassengerArrivalRate::new((direction_offset + pair_variation) as i64)
        .expect("the fixed seeded demand rate must remain positive")
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
    use crate::{
        model::{
            ConstructionDifficulty, DistanceMetres, DurationSeconds, Electrification,
            InfrastructureProject, InfrastructureProjectFunding, InfrastructureProjectId,
            InfrastructureProjectKind, InfrastructureProjectStatus, InfrastructureProjectTimeline,
            Money, PassengerArrivalRate, PlannedRailLine, PlannedRailStation, RailLine, RailLineId,
            RailStation, RailStationId, SpeedKilometresPerHour, TrackCount, UtcSeconds,
        },
        sim::world::create_new_game,
    };

    use super::{
        effective_arrival_rate_per_hour, replenish_directional_demand,
        synchronize_directional_demand_with_network,
    };

    fn game() -> crate::model::GameState {
        create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0))
    }

    #[test]
    fn creates_twelve_directional_pools_with_asymmetric_rates() {
        let game = game();

        assert_eq!(game.origin_destination_demand.len(), 12);
        for pool in &game.origin_destination_demand {
            let reverse = game
                .origin_destination_demand
                .iter()
                .find(|other| {
                    other.origin_station_id == pool.destination_station_id
                        && other.destination_station_id == pool.origin_station_id
                })
                .expect("every pool must have its reverse direction");
            assert_ne!(
                pool.passenger_arrival_rate_per_hour,
                reverse.passenger_arrival_rate_per_hour
            );
            assert_eq!(pool.market_maturity.basis_points(), 2_500);
        }
    }

    #[test]
    fn starter_network_has_neutral_infrastructure_attractiveness() {
        let game = game();

        for pool in &game.origin_destination_demand {
            assert_eq!(
                effective_arrival_rate_per_hour(&game, pool),
                pool.passenger_arrival_rate_per_hour.passengers_per_hour()
            );
        }
    }

    #[test]
    fn faster_infrastructure_increases_effective_demand_without_rewriting_base_rate() {
        let mut game = game();
        for line in &mut game.region.rail_authority.rail_network.rail_lines {
            line.speed_limit = SpeedKilometresPerHour::new(140).unwrap();
        }
        game.origin_destination_demand[0].waiting_passengers = 0;
        game.origin_destination_demand[0].passenger_arrival_rate_per_hour =
            PassengerArrivalRate::new(8).unwrap();
        game.origin_destination_demand[0].fractional_passenger_seconds = 0;

        assert_eq!(
            effective_arrival_rate_per_hour(&game, &game.origin_destination_demand[0]),
            10
        );

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(3_600));

        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 10);
        assert_eq!(
            game.origin_destination_demand[0]
                .passenger_arrival_rate_per_hour
                .passengers_per_hour(),
            8
        );
    }

    #[test]
    fn preserves_fractional_accumulation_across_updates() {
        let mut game = game();
        let pool = &mut game.origin_destination_demand[0];
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour = PassengerArrivalRate::new(3).unwrap();
        pool.fractional_passenger_seconds = 0;

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(600));
        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 0);
        assert_eq!(
            game.origin_destination_demand[0].fractional_passenger_seconds,
            1_800
        );

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(1_200));
        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 1);
        assert_eq!(
            game.origin_destination_demand[0].fractional_passenger_seconds,
            0
        );
    }

    #[test]
    fn caps_demand_and_discards_overflow() {
        let mut game = game();
        game.rules.demand.cap_duration = DurationSeconds::from_seconds(3_600);
        let pool = &mut game.origin_destination_demand[0];
        pool.waiting_passengers = 2;
        pool.passenger_arrival_rate_per_hour = PassengerArrivalRate::new(3).unwrap();
        pool.fractional_passenger_seconds = 3_000;

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(600));
        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 3);
        assert_eq!(
            game.origin_destination_demand[0].fractional_passenger_seconds,
            0
        );

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(1_200));
        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 3);
        assert_eq!(
            game.origin_destination_demand[0].fractional_passenger_seconds,
            0
        );
    }

    #[test]
    fn repeated_or_backward_timestamps_do_not_replenish_twice() {
        let mut game = game();
        game.origin_destination_demand[0].waiting_passengers = 0;
        game.origin_destination_demand[0].passenger_arrival_rate_per_hour =
            PassengerArrivalRate::new(6).unwrap();

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(600));
        let after_first = game.origin_destination_demand[0].clone();
        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(600));
        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(300));

        assert_eq!(game.origin_destination_demand[0], after_first);
        assert_eq!(game.last_processed_at, UtcSeconds::from_unix_seconds(600));
    }

    #[test]
    fn a_large_time_gap_uses_elapsed_time_arithmetic_and_caps() {
        let mut game = game();
        game.rules.demand.cap_duration = DurationSeconds::from_seconds(24 * 60 * 60);
        let pool = &mut game.origin_destination_demand[0];
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour = PassengerArrivalRate::new(7).unwrap();
        pool.fractional_passenger_seconds = 0;

        replenish_directional_demand(&mut game, UtcSeconds::from_unix_seconds(i64::MAX));

        assert_eq!(game.origin_destination_demand[0].waiting_passengers, 168);
        assert_eq!(
            game.origin_destination_demand[0].fractional_passenger_seconds,
            0
        );
        assert_eq!(
            game.last_processed_at,
            UtcSeconds::from_unix_seconds(i64::MAX)
        );
    }

    #[test]
    fn pools_only_reference_connected_rail_stations() {
        let game = game();
        let station_ids = game
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .map(|station| station.id)
            .collect::<Vec<_>>();

        assert!(game.origin_destination_demand.iter().all(|pool| {
            station_ids.contains(&pool.origin_station_id)
                && station_ids.contains(&pool.destination_station_id)
                && pool.origin_station_id != pool.destination_station_id
        }));
        assert!(!station_ids.contains(&RailStationId::new(5)));
    }

    #[test]
    fn newly_connected_station_creates_both_directional_markets_without_duplicates() {
        let mut game = game();
        let existing_station_ids = game
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .map(|station| station.id)
            .collect::<Vec<_>>();
        let settlement_id = game.region.settlements[4].id;
        let new_station_id = RailStationId::new_v4();
        let new_line_id = RailLineId::new_v4();
        let opened_at = UtcSeconds::from_unix_seconds(3_600);

        game.region
            .rail_authority
            .rail_network
            .rail_stations
            .push(RailStation {
                id: new_station_id,
                settlement_id,
            });
        game.region
            .rail_authority
            .rail_network
            .rail_lines
            .push(RailLine {
                id: new_line_id,
                first_station_id: existing_station_ids[0],
                second_station_id: new_station_id,
                distance: DistanceMetres::new(20_000).unwrap(),
                speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                track_count: TrackCount::SINGLE,
                electrification: Electrification::None,
                construction_difficulty: ConstructionDifficulty::Moderate,
            });
        game.region
            .rail_authority
            .infrastructure_projects
            .push(InfrastructureProject {
                id: InfrastructureProjectId::new_v4(),
                kind: InfrastructureProjectKind::NewLine {
                    planned_stations: vec![PlannedRailStation {
                        id: new_station_id,
                        settlement_id,
                    }],
                    planned_lines: vec![PlannedRailLine {
                        id: new_line_id,
                        first_station_id: existing_station_ids[0],
                        second_station_id: new_station_id,
                        distance: DistanceMetres::new(20_000).unwrap(),
                        speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                        track_count: TrackCount::SINGLE,
                        electrification: Electrification::None,
                        construction_difficulty: ConstructionDifficulty::Moderate,
                    }],
                },
                status: InfrastructureProjectStatus::Open,
                timeline: InfrastructureProjectTimeline {
                    requested_at: UtcSeconds::from_unix_seconds(0),
                    review_started_at: None,
                    proposed_at: None,
                    approved_at: None,
                    funding_completed_at: None,
                    scheduled_start_at: None,
                    construction_started_at: None,
                    planned_completion_at: Some(opened_at),
                    completed_at: Some(opened_at),
                    deferred_at: None,
                    cancelled_at: None,
                },
                funding: InfrastructureProjectFunding {
                    estimated_cost: Money::ZERO,
                    authority_committed: Money::ZERO,
                    operator_contributed: Money::ZERO,
                    access_fee_credit_awarded: Money::ZERO,
                    access_fee_credit_remaining: Money::ZERO,
                },
            });

        synchronize_directional_demand_with_network(&mut game, opened_at);
        assert_eq!(game.origin_destination_demand.len(), 20);
        for existing_station_id in existing_station_ids {
            assert!(game.origin_destination_demand.iter().any(|pool| {
                pool.origin_station_id == new_station_id
                    && pool.destination_station_id == existing_station_id
            }));
            assert!(game.origin_destination_demand.iter().any(|pool| {
                pool.origin_station_id == existing_station_id
                    && pool.destination_station_id == new_station_id
            }));
        }

        synchronize_directional_demand_with_network(&mut game, opened_at);
        assert_eq!(game.origin_destination_demand.len(), 20);
    }

    #[test]
    fn new_market_demand_ramps_before_reaching_mature_rate() {
        let mut game = game();
        let existing_station_id = game.region.rail_authority.rail_network.rail_stations[0].id;
        let settlement_id = game.region.settlements[4].id;
        let new_station_id = RailStationId::new_v4();
        let opened_at = UtcSeconds::from_unix_seconds(0);

        game.region
            .rail_authority
            .rail_network
            .rail_stations
            .push(RailStation {
                id: new_station_id,
                settlement_id,
            });
        game.region
            .rail_authority
            .infrastructure_projects
            .push(InfrastructureProject {
                id: InfrastructureProjectId::new_v4(),
                kind: InfrastructureProjectKind::NewLine {
                    planned_stations: vec![PlannedRailStation {
                        id: new_station_id,
                        settlement_id,
                    }],
                    planned_lines: vec![],
                },
                status: InfrastructureProjectStatus::Open,
                timeline: InfrastructureProjectTimeline {
                    requested_at: opened_at,
                    review_started_at: None,
                    proposed_at: None,
                    approved_at: None,
                    funding_completed_at: None,
                    scheduled_start_at: None,
                    construction_started_at: None,
                    planned_completion_at: Some(opened_at),
                    completed_at: Some(opened_at),
                    deferred_at: None,
                    cancelled_at: None,
                },
                funding: InfrastructureProjectFunding {
                    estimated_cost: Money::ZERO,
                    authority_committed: Money::ZERO,
                    operator_contributed: Money::ZERO,
                    access_fee_credit_awarded: Money::ZERO,
                    access_fee_credit_remaining: Money::ZERO,
                },
            });

        synchronize_directional_demand_with_network(
            &mut game,
            UtcSeconds::from_unix_seconds(2 * 60 * 60),
        );
        let pool = game
            .origin_destination_demand
            .iter()
            .find(|pool| {
                pool.origin_station_id == new_station_id
                    && pool.destination_station_id == existing_station_id
            })
            .unwrap();
        let mature_two_hour_arrivals = pool
            .passenger_arrival_rate_per_hour
            .passengers_per_hour()
            .saturating_mul(2);
        assert!(pool.waiting_passengers < mature_two_hour_arrivals);
        assert!(pool.waiting_passengers > 0);
    }
}

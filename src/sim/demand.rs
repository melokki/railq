//! Elapsed-time replenishment for directional origin-destination demand.

use crate::model::{
    GameState, OriginDestinationDemand, PassengerArrivalRate, RailStationId, Region, UtcSeconds,
};

const SECONDS_PER_HOUR: u128 = 60 * 60;
const INITIAL_DEMAND_HOURS: u32 = 3;

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
    let elapsed_seconds = (effective_now.unix_seconds() as i128
        - state.last_processed_at.unix_seconds() as i128) as u128;
    let cap_duration_seconds = state.rules.demand.cap_duration.seconds();

    for pool in &mut state.origin_destination_demand {
        replenish_pool(pool, elapsed_seconds, cap_duration_seconds);
    }
    state.last_processed_at = effective_now;
}

fn replenish_pool(
    pool: &mut OriginDestinationDemand,
    elapsed_seconds: u128,
    cap_duration_seconds: u64,
) {
    let rate = u128::from(pool.passenger_arrival_rate_per_hour.passengers_per_hour());
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
        model::{DurationSeconds, PassengerArrivalRate, RailStationId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::replenish_directional_demand;

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
        }
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
}

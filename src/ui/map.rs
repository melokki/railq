//! Text presentation of the Region map.
//!
//! The map reads the public Rail Network and the Player Company's operating
//! state without changing either. Markers make connection state understandable
//! in terminals where colour is unavailable.

use std::fmt::Write;

use crate::model::{GameState, Journey, Money, RailStationId, TrainStatus, UtcSeconds};

/// Renders the current Region, Rail Network, Fleet locations, and Company Funds.
pub fn render(state: &GameState) -> String {
    render_at(state, state.last_processed_at)
}

/// Renders the map using `now` to calculate active Journey progress and ETA.
pub fn render_at(state: &GameState, now: UtcSeconds) -> String {
    let network = &state.region.rail_authority.rail_network;
    let mut output = String::new();

    writeln!(output, "Region: {}", state.region.name).expect("writing to a String cannot fail");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Legend: [S] Connected Settlement / Rail Station; [ ] Unconnected Settlement"
    )
    .expect("writing to a String cannot fail");

    writeln!(output, "\nRail Lines:").expect("writing to a String cannot fail");
    for line in &network.rail_lines {
        let first = station_label(state, line.first_station_id);
        let second = station_label(state, line.second_station_id);
        writeln!(
            output,
            "  Rail Line {}: [{first}] -- {} -- [{second}]",
            line.id.get(),
            format_distance(line.distance.metres()),
        )
        .expect("writing to a String cannot fail");
    }

    writeln!(output, "Settlements:").expect("writing to a String cannot fail");
    for settlements in state.region.settlements.chunks(4) {
        let mut first = true;
        output.push_str("  ");
        for settlement in settlements {
            if !first {
                output.push_str(" | ");
            }
            first = false;
            let marker = if network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
            {
                "[S]"
            } else {
                "[ ]"
            };
            write!(output, "{marker} {}", settlement.name)
                .expect("writing to a String cannot fail");
        }
        output.push('\n');
    }

    writeln!(output, "Trains:").expect("writing to a String cannot fail");
    if state.player_company.fleet.trains.is_empty() {
        writeln!(output, "  No Trains in the Fleet.").expect("writing to a String cannot fail");
    }
    for train in &state.player_company.fleet.trains {
        match train.status {
            TrainStatus::Ready { at } => {
                writeln!(
                    output,
                    "  Train {} ({}) — READY at {}",
                    train.id.get(),
                    train.model_name,
                    station_label(state, at),
                )
                .expect("writing to a String cannot fail");
            }
            TrainStatus::Travelling { journey_id } => {
                let journey = state
                    .active_journeys
                    .iter()
                    .find(|journey| journey.id == journey_id);
                match journey {
                    Some(journey) => render_travelling_train(
                        &mut output,
                        state,
                        train.id.get(),
                        &train.model_name,
                        journey,
                        now,
                    ),
                    None => writeln!(
                        output,
                        "  Train {} ({}) — TRAVELLING on Journey {} (details unavailable)",
                        train.id.get(),
                        train.model_name,
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }

    output
}

fn render_travelling_train(
    output: &mut String,
    state: &GameState,
    train_id: u64,
    model_name: &str,
    journey: &Journey,
    now: UtcSeconds,
) {
    writeln!(
        output,
        "  Train {train_id} ({model_name}) — TRAVELLING {} -> {} | progress: {}% | ETA: {}",
        station_label(state, journey.origin_station_id),
        station_label(state, journey.destination_station_id),
        journey_progress_percent(journey, now),
        format_duration(remaining_seconds(journey, now)),
    )
    .expect("writing to a String cannot fail");
}

fn station_label(state: &GameState, station_id: RailStationId) -> &str {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return "unknown Rail Station";
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map_or("unknown Settlement", |settlement| settlement.name.as_str())
}

fn journey_progress_percent(journey: &Journey, now: UtcSeconds) -> u64 {
    let duration = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds());
    if duration <= 0 {
        return 100;
    }
    let elapsed = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, duration);
    u64::try_from(elapsed.saturating_mul(100) / duration).unwrap_or(100)
}

fn remaining_seconds(journey: &Journey, now: UtcSeconds) -> u64 {
    u64::try_from(
        journey
            .arrives_at
            .unix_seconds()
            .saturating_sub(now.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

fn format_distance(metres: u64) -> String {
    let kilometres = metres / 1_000;
    let remainder = metres % 1_000;
    if remainder == 0 {
        format!("{kilometres} km")
    } else {
        format!("{kilometres}.{remainder:03} km")
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::render_at;

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    #[test]
    fn map_shows_every_settlement_and_the_seeded_rail_lines_without_colour() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let map = render_at(&state, STARTED_AT);

        for station in &state.region.rail_authority.rail_network.rail_stations {
            let settlement = state
                .region
                .settlements
                .iter()
                .find(|settlement| settlement.id == station.settlement_id)
                .unwrap();
            assert!(map.contains(&format!("[S] {}", settlement.name)));
        }
        let unconnected_count = state
            .region
            .settlements
            .iter()
            .filter(|settlement| {
                !state
                    .region
                    .rail_authority
                    .rail_network
                    .rail_stations
                    .iter()
                    .any(|station| station.settlement_id == settlement.id)
            })
            .filter(|settlement| map.contains(&format!("[ ] {}", settlement.name)))
            .count();
        assert_eq!(unconnected_count, 6);
        for settlement in &state.region.settlements {
            assert!(map.contains(&settlement.name));
        }
        for line in &state.region.rail_authority.rail_network.rail_lines {
            assert!(map.contains(&format!("Rail Line {}", line.id.get())));
        }
        assert!(map.contains("Company Funds: $"));
    }

    #[test]
    fn map_shows_ready_and_travelling_train_locations_progress_and_eta() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(2);
        let train_id = purchase_train(&mut state, 0, origin).unwrap();

        let ready_map = render_at(&state, STARTED_AT);
        assert!(ready_map.contains(&format!("Train {}", train_id.get())));
        assert!(ready_map.contains("READY at"));

        let service_id = find_or_create_service(&mut state, origin, destination).unwrap();
        let departed_at = UtcSeconds::from_unix_seconds(1_100);
        dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
        let journey = &state.active_journeys[0];
        let halfway = UtcSeconds::from_unix_seconds(
            journey.departed_at.unix_seconds()
                + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
        );

        let travelling_map = render_at(&state, halfway);
        assert!(travelling_map.contains("TRAVELLING"));
        assert!(travelling_map.contains("progress: 50%"));
        assert!(travelling_map.contains("ETA:"));
        assert!(travelling_map.contains("Alden") || travelling_map.contains("Bellhaven"));
    }
}

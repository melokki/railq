//! Seeded Region generation and new Player Company setup.

use rand_chacha::{
    ChaCha8Rng,
    rand_core::{Rng, SeedableRng},
};

use crate::{
    balance::BalanceConfig,
    model::{
        ConstructionDifficulty, DemandRules, Electrification, Financials, Fleet, GameRules,
        GameState, Money, PlayerCompany, RailAuthority, RailLine, RailLineId, RailNetwork,
        RailStation, RailStationId, RailwayRegistration, Region, Settlement, SettlementId,
        SpeedKilometresPerHour, TrackCount, UtcSeconds, VehicleKeeperMark,
    },
    sim::demand::seed_directional_demand,
};

const SETTLEMENT_NAMES: [&str; 16] = [
    "Alden",
    "Bellhaven",
    "Cedarfall",
    "Dunmere",
    "Eastmere",
    "Fairford",
    "Glenhaven",
    "Highvale",
    "Iverton",
    "Juniper",
    "Kestrel",
    "Larkspur",
    "Marlowe",
    "Northgate",
    "Oakridge",
    "Pinewatch",
];
const REGION_FORMS: [&str; 4] = ["Federation", "Union", "Commonwealth", "Confederation"];

#[derive(Clone, Copy)]
struct RegionIdentity {
    name: &'static str,
    registration_code: u8,
    registration_mark: &'static str,
}

const REGION_IDENTITIES: [RegionIdentity; 8] = [
    RegionIdentity {
        name: "Varelia",
        registration_code: 67,
        registration_mark: "VA",
    },
    RegionIdentity {
        name: "Ardinia",
        registration_code: 68,
        registration_mark: "AR",
    },
    RegionIdentity {
        name: "Estara",
        registration_code: 69,
        registration_mark: "ES",
    },
    RegionIdentity {
        name: "Norvia",
        registration_code: 70,
        registration_mark: "NV",
    },
    RegionIdentity {
        name: "Caldria",
        registration_code: 71,
        registration_mark: "CA",
    },
    RegionIdentity {
        name: "Meridia",
        registration_code: 72,
        registration_mark: "ME",
    },
    RegionIdentity {
        name: "Solenne",
        registration_code: 73,
        registration_mark: "SO",
    },
    RegionIdentity {
        name: "Tavora",
        registration_code: 74,
        registration_mark: "TA",
    },
];
const CONNECTED_SETTLEMENT_COUNT: usize = 4;
const SETTLEMENT_COUNT: usize = 10;

/// Generates the fixed initial Rail Network and its seeded Region details.
///
/// The first four Settlements receive one Rail Station each. Their Rail Lines
/// form the initial tree A-B-C plus B-D; the remaining six Settlements are
/// deliberately unconnected.
pub fn generate_region(seed: u64) -> Region {
    let mut random = ChaCha8Rng::seed_from_u64(seed);
    let region_form = choose(&mut random, &REGION_FORMS);
    let identity = REGION_IDENTITIES[(random.next_u64() as usize) % REGION_IDENTITIES.len()];
    let name = format!("{region_form} of {}", identity.name);
    let settlement_names = select_settlement_names(&mut random);
    let settlements = settlement_names
        .into_iter()
        .enumerate()
        .map(|(index, settlement_name)| Settlement {
            id: SettlementId::new((index + 1) as u64),
            name: settlement_name.to_owned(),
            population: 40_000 + random.next_u64() % 260_001,
        })
        .collect::<Vec<_>>();
    let population = settlements
        .iter()
        .map(|settlement| settlement.population)
        .sum();

    let rail_stations = settlements
        .iter()
        .take(CONNECTED_SETTLEMENT_COUNT)
        .enumerate()
        .map(|(index, settlement)| RailStation {
            id: RailStationId::new((index + 1) as u64),
            settlement_id: settlement.id,
        })
        .collect();
    let rail_lines = vec![
        rail_line(1, 1, 2, 10_000),
        rail_line(2, 2, 3, 42_000),
        rail_line(3, 2, 4, 31_000),
    ];

    Region {
        name: name.clone(),
        railway_registration: RailwayRegistration {
            numeric_code: identity.registration_code,
            mark: identity.registration_mark.into(),
        },
        population,
        settlements,
        rail_authority: RailAuthority {
            name: format!("{name} Rail Authority"),
            rail_network: RailNetwork {
                rail_stations,
                rail_lines,
            },
        },
    }
}

/// Returns the stable fictional railway registration identity for an existing Region.
///
/// Old saves did not persist this value, so migrations recover it from the
/// generated Region name. Unknown/custom Region names receive a deterministic
/// RailQ fallback based on the saved world seed.
pub fn railway_registration_for_existing_region(
    region_name: &str,
    world_seed: u64,
) -> RailwayRegistration {
    if let Some(identity) = REGION_IDENTITIES
        .iter()
        .find(|identity| region_name == identity.name || region_name.ends_with(identity.name))
    {
        return RailwayRegistration {
            numeric_code: identity.registration_code,
            mark: identity.registration_mark.into(),
        };
    }

    RailwayRegistration {
        numeric_code: 80 + (world_seed % 20) as u8,
        mark: "RQ".into(),
    }
}

/// Creates a fresh game for a named Player Company without performing I/O.
pub fn create_new_game(
    world_seed: u64,
    company_name: impl Into<String>,
    started_at: UtcSeconds,
) -> GameState {
    let balance = BalanceConfig::provisional();
    let region = generate_region(world_seed);
    let company_name = company_name.into();
    let vehicle_keeper_mark = VehicleKeeperMark::generated_from_company_name(&company_name);
    GameState {
        world_seed,
        origin_destination_demand: seed_directional_demand(&region, world_seed),
        region,
        player_company: PlayerCompany {
            name: company_name,
            vehicle_keeper_mark,
            funds: balance.starting_company_funds(),
            fleet: Fleet::default(),
            passenger_services: vec![],
        },
        active_journeys: vec![],
        financials: Financials {
            operating_revenue: Money::ZERO,
            infrastructure_access_fees: Money::ZERO,
            fuel_costs: Money::ZERO,
            recent_journey_receipts: vec![],
        },
        rules: GameRules {
            balance,
            demand: DemandRules::provisional(),
        },
        last_processed_at: started_at,
    }
}

fn choose<'a>(random: &mut ChaCha8Rng, choices: &'a [&'a str]) -> &'a str {
    choices[(random.next_u64() as usize) % choices.len()]
}

fn select_settlement_names(random: &mut ChaCha8Rng) -> [&'static str; SETTLEMENT_COUNT] {
    let mut names = SETTLEMENT_NAMES;
    for current in (1..names.len()).rev() {
        let selected = (random.next_u64() as usize) % (current + 1);
        names.swap(current, selected);
    }
    names[..SETTLEMENT_COUNT]
        .try_into()
        .expect("the seeded settlement name pool must contain ten names")
}

fn rail_line(
    id: u64,
    first_station_id: u64,
    second_station_id: u64,
    distance_metres: i64,
) -> RailLine {
    RailLine {
        id: RailLineId::new(id),
        first_station_id: RailStationId::new(first_station_id),
        second_station_id: RailStationId::new(second_station_id),
        distance: crate::model::DistanceMetres::new(distance_metres)
            .expect("the fixed starter Rail Line distance must be positive"),
        speed_limit: SpeedKilometresPerHour::new(70)
            .expect("the starter Rail Line speed limit must be positive"),
        track_count: TrackCount::SINGLE,
        electrification: Electrification::None,
        construction_difficulty: ConstructionDifficulty::Moderate,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn same_seed_produces_the_same_region() {
        assert_eq!(generate_region(42), generate_region(42));
        assert_ne!(generate_region(42), generate_region(43));
    }

    #[test]
    fn generated_region_has_stable_railway_registration_identity() {
        let region = generate_region(42);
        let registration = &region.railway_registration;

        assert!((10..=99).contains(&registration.numeric_code));
        assert_eq!(registration.display_code().len(), 2);
        assert_eq!(registration.mark.len(), 2);
        assert!(
            registration
                .mark
                .chars()
                .all(|character| character.is_ascii_uppercase())
        );
        assert_eq!(
            railway_registration_for_existing_region(&region.name, 42),
            registration.clone()
        );
    }

    #[test]
    fn generated_region_has_the_fixed_starter_topology() {
        let region = generate_region(42);
        let network = &region.rail_authority.rail_network;

        assert_eq!(region.settlements.len(), SETTLEMENT_COUNT);
        assert_eq!(network.rail_stations.len(), CONNECTED_SETTLEMENT_COUNT);
        assert_eq!(network.rail_lines.len(), 3);
        assert_eq!(
            network
                .rail_lines
                .iter()
                .map(|line| (line.first_station_id.get(), line.second_station_id.get()))
                .collect::<Vec<_>>(),
            vec![(1, 2), (2, 3), (2, 4)]
        );
        assert!(
            network
                .rail_lines
                .iter()
                .all(|line| line.distance.metres() > 0)
        );
        assert!(
            network
                .rail_lines
                .iter()
                .any(|line| line.distance.metres() == 10_000)
        );
        assert!(network.rail_lines.iter().all(|line| {
            line.speed_limit.kilometres_per_hour() == 70
                && line.track_count == TrackCount::SINGLE
                && line.electrification == Electrification::None
                && line.construction_difficulty == ConstructionDifficulty::Moderate
        }));
    }

    #[test]
    fn population_ids_and_references_are_valid() {
        let region = generate_region(42);
        let network = &region.rail_authority.rail_network;
        let settlement_ids = region
            .settlements
            .iter()
            .map(|settlement| settlement.id)
            .collect::<HashSet<_>>();
        let station_ids = network
            .rail_stations
            .iter()
            .map(|station| station.id)
            .collect::<HashSet<_>>();
        let line_ids = network
            .rail_lines
            .iter()
            .map(|line| line.id)
            .collect::<HashSet<_>>();

        assert_eq!(settlement_ids.len(), SETTLEMENT_COUNT);
        assert_eq!(station_ids.len(), CONNECTED_SETTLEMENT_COUNT);
        assert_eq!(line_ids.len(), 3);
        assert_eq!(
            region.population,
            region
                .settlements
                .iter()
                .map(|settlement| settlement.population)
                .sum()
        );
        assert!(
            network
                .rail_stations
                .iter()
                .all(|station| settlement_ids.contains(&station.settlement_id))
        );
        assert!(network.rail_lines.iter().all(|line| {
            station_ids.contains(&line.first_station_id)
                && station_ids.contains(&line.second_station_id)
                && line.first_station_id != line.second_station_id
        }));
    }

    #[test]
    fn new_game_keeps_its_seed_and_named_player_company() {
        let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
        let game = create_new_game(42, "Alden Passenger", started_at);

        assert_eq!(game.world_seed, 42);
        assert_eq!(game.player_company.name, "Alden Passenger");
        assert_eq!(
            game.player_company.funds,
            game.rules.balance.starting_company_funds()
        );
        assert_eq!(game.last_processed_at, started_at);
        assert_eq!(game.region, generate_region(game.world_seed));
    }
}

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    model::{RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train,
        journeys::dispatch_journey,
        services::{assign_train_to_service, find_or_create_service},
        time::advance_time,
        world::create_new_game,
    },
    storage::{legacy::decode_legacy_game_state, migrations::ensure_schema},
};

use super::*;

static NEXT_TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "railq-sqlite-storage-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn save_path(&self) -> PathBuf {
        self.path.join("company.db")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn active_game() -> GameState {
    let departed_at = UtcSeconds::from_unix_seconds(1_000);
    let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    state.player_company.fleet.trains[0].nickname =
        Some(TrainNickname::parse("Morning Star").unwrap());
    let service_id =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2)).unwrap();
    assign_train_to_service(&mut state, train_id, service_id).unwrap();
    dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
    state.origin_destination_demand[0].market_maturity =
        MarketMaturity::from_basis_points(4_321).unwrap();
    state.origin_destination_demand[0].fractional_passenger_seconds = 1_234;
    state
}

#[test]
fn migrated_bidirectional_journey_snapshot_remains_loadable() {
    let directory = TestDirectory::new();
    let slot = SaveSlot::open(directory.save_path()).unwrap();
    let mut state = active_game();

    // Before schema v3, the same Passenger Service could be dispatched in
    // either direction. Simulate a migrated active Journey whose saved
    // direction is the reverse of the new directional Service order.
    state.player_company.passenger_services[0]
        .stop_station_ids
        .reverse();
    state.player_company.passenger_services[0]
        .rail_line_ids
        .reverse();
    state.active_journeys[0].current_stop_index = state.player_company.passenger_services[0]
        .stop_station_ids
        .len()
        .saturating_sub(1);

    slot.save(&state).unwrap();

    assert_eq!(slot.load().unwrap(), Some(state));
}

#[test]
fn sqlite_round_trips_all_current_operating_state() {
    let directory = TestDirectory::new();
    let slot = SaveSlot::open(directory.save_path()).unwrap();
    let mut state = active_game();
    state.player_company.passenger_services[0].custom_name = Some("Capital Link".into());
    state.player_company.passenger_services[0].direction_mode = ServiceDirectionMode::ForwardOnly;
    state.player_company.passenger_services[0].reverse_train_number = None;
    state.region.rail_authority.construction_capacity = 2;
    state.region.bulletin.push(BulletinEntry {
        occurred_at: UtcSeconds::from_unix_seconds(12_345),
        category: BulletinCategory::Authority,
        headline: "Alden connection approved".into(),
        detail: "The regional case passed formal review.".into(),
    });
    state.region.rail_authority.finances = RailAuthorityFinances {
        treasury: Money::from_cents(9_000_000),
        maintenance_reserve: Money::from_cents(1_500_000),
        committed_investment: Money::from_cents(2_000_000),
        carried_over_funds: Money::from_cents(750_000),
        regional_public_allocation: Money::from_cents(3_000_000),
        infrastructure_access_fee_revenue: Money::from_cents(425_000),
        next_fiscal_period_at: Some(UtcSeconds::from_unix_seconds(25_000)),
    };

    slot.save(&state).unwrap();

    assert_eq!(slot.load().unwrap(), Some(state));
}

#[test]
fn sqlite_round_trips_journey_receipt_service_telemetry() {
    let directory = TestDirectory::new();
    let slot = SaveSlot::open(directory.save_path()).unwrap();
    let mut state = active_game();
    let service_id = state.player_company.passenger_services[0].id;
    let arrives_at = state.active_journeys[0].arrives_at;

    advance_time(&mut state, arrives_at).unwrap();

    let receipt = state.financials.recent_journey_receipts[0].clone();
    assert_eq!(receipt.service_id, Some(service_id));
    assert_eq!(receipt.service_code.as_deref(), Some("R1"));
    assert_eq!(receipt.purpose, Some(JourneyPurpose::RevenueService));
    assert_eq!(receipt.departed_at, Some(UtcSeconds::from_unix_seconds(1_000)));

    slot.save(&state).unwrap();
    let loaded = slot.load().unwrap().unwrap();

    assert_eq!(loaded.financials.recent_journey_receipts, vec![receipt]);
}

#[test]
fn sqlite_round_trips_infrastructure_access_discount() {
    let directory = TestDirectory::new();
    let slot = SaveSlot::open(directory.save_path()).unwrap();
    let mut state = active_game();
    let rail_line_id = state.region.rail_authority.rail_network.rail_lines[0].id;
    state.region.rail_authority.infrastructure_projects = vec![InfrastructureProject {
        id: InfrastructureProjectId::new_v4(),
        kind: InfrastructureProjectKind::Renewal {
            rail_line_ids: vec![rail_line_id],
        },
        status: InfrastructureProjectStatus::Open,
        timeline: InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(1_000),
            review_started_at: Some(UtcSeconds::from_unix_seconds(1_100)),
            proposed_at: Some(UtcSeconds::from_unix_seconds(1_200)),
            approved_at: Some(UtcSeconds::from_unix_seconds(1_300)),
            funding_completed_at: Some(UtcSeconds::from_unix_seconds(1_400)),
            scheduled_start_at: Some(UtcSeconds::from_unix_seconds(1_500)),
            construction_started_at: Some(UtcSeconds::from_unix_seconds(1_600)),
            planned_completion_at: Some(UtcSeconds::from_unix_seconds(1_700)),
            completed_at: Some(UtcSeconds::from_unix_seconds(1_700)),
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        },
        funding: InfrastructureProjectFunding {
            estimated_cost: Money::from_cents(1_000),
            authority_committed: Money::from_cents(900),
            operator_contributed: Money::from_cents(100),
            access_fee_discount: Some(InfrastructureAccessDiscount {
                basis_points: 5_000,
                expires_at: UtcSeconds::from_unix_seconds(691_200),
            }),
        },
    }];

    slot.save(&state).unwrap();

    assert_eq!(slot.load().unwrap(), Some(state));
}

#[test]
fn sqlite_round_trips_all_infrastructure_project_kinds() {
    let directory = TestDirectory::new();
    let slot = SaveSlot::open(directory.save_path()).unwrap();
    let mut state = active_game();
    let timeline = |requested_at| InfrastructureProjectTimeline {
        requested_at: UtcSeconds::from_unix_seconds(requested_at),
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
    let mut reconsidered_timeline = timeline(2_004);
    reconsidered_timeline.reconsideration_count = 2;
    state.region.rail_authority.infrastructure_projects = vec![
        InfrastructureProject {
            id: InfrastructureProjectId::new(1),
            kind: InfrastructureProjectKind::NewLine {
                planned_stations: vec![PlannedRailStation {
                    id: RailStationId::new(5),
                    settlement_id: SettlementId::new(5),
                }],
                planned_lines: vec![PlannedRailLine {
                    id: RailLineId::new(4),
                    first_station_id: RailStationId::new(4),
                    second_station_id: RailStationId::new(5),
                    distance: DistanceMetres::new(12_000).unwrap(),
                    speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                    track_count: TrackCount::SINGLE,
                    electrification: Electrification::None,
                    construction_difficulty: ConstructionDifficulty::Moderate,
                }],
            },
            status: InfrastructureProjectStatus::Requested,
            timeline: timeline(2_000),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(2),
            kind: InfrastructureProjectKind::SpeedUpgrade {
                rail_line_ids: vec![RailLineId::new(1)],
                target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
            },
            status: InfrastructureProjectStatus::UnderReview,
            timeline: timeline(2_001),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(3),
            kind: InfrastructureProjectKind::DoubleTracking {
                rail_line_ids: vec![RailLineId::new(2)],
                target_track_count: TrackCount::DOUBLE,
            },
            status: InfrastructureProjectStatus::Proposed,
            timeline: timeline(2_002),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(4),
            kind: InfrastructureProjectKind::Electrification {
                rail_line_ids: vec![RailLineId::new(3)],
            },
            status: InfrastructureProjectStatus::Approved,
            timeline: timeline(2_003),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(5),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Deferred,
            timeline: reconsidered_timeline,
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(6),
            kind: InfrastructureProjectKind::StationUpgrade {
                rail_station_ids: vec![RailStationId::new(1)],
            },
            status: InfrastructureProjectStatus::Funding,
            timeline: timeline(2_005),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(7),
            kind: InfrastructureProjectKind::StationUpgrade {
                rail_station_ids: vec![RailStationId::new(2)],
            },
            status: InfrastructureProjectStatus::Rejected,
            timeline: timeline(2_006),
            funding: InfrastructureProjectFunding::default(),
        },
    ];
    slot.save(&state).unwrap();

    assert_eq!(slot.load().unwrap(), Some(state));
}

#[test]
fn validation_rejects_conflicting_projects_under_construction() {
    let mut state = active_game();
    let timeline = InfrastructureProjectTimeline {
        requested_at: UtcSeconds::from_unix_seconds(2_000),
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
    state.region.rail_authority.construction_capacity = 2;
    state.region.rail_authority.infrastructure_projects = vec![
        InfrastructureProject {
            id: InfrastructureProjectId::new(1),
            kind: InfrastructureProjectKind::SpeedUpgrade {
                rail_line_ids: vec![RailLineId::new(1)],
                target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(2),
            kind: InfrastructureProjectKind::Electrification {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        },
    ];
    assert_eq!(
        validate_game_state(&state),
        Err(SaveValidationError::ImpossibleState {
            reason: "conflicting Infrastructure Projects are simultaneously under construction",
        })
    );
}

#[test]
fn validation_allows_parallel_construction_on_disjoint_rail_lines() {
    let mut state = active_game();
    let timeline = InfrastructureProjectTimeline {
        requested_at: UtcSeconds::from_unix_seconds(2_000),
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
    state.region.rail_authority.construction_capacity = 2;
    state.region.rail_authority.infrastructure_projects = vec![
        InfrastructureProject {
            id: InfrastructureProjectId::new(1),
            kind: InfrastructureProjectKind::SpeedUpgrade {
                rail_line_ids: vec![RailLineId::new(1)],
                target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(2),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(2)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        },
    ];
    assert_eq!(validate_game_state(&state), Ok(()));
}

#[test]
fn validation_allows_open_project_to_keep_historical_authority_commitment() {
    let mut state = active_game();
    let historical_commitment = Money::from_cents(250_000);
    state.region.rail_authority.finances.committed_investment = Money::ZERO;
    state.region.rail_authority.infrastructure_projects = vec![InfrastructureProject {
        id: InfrastructureProjectId::new_v4(),
        kind: InfrastructureProjectKind::Renewal {
            rail_line_ids: vec![state.region.rail_authority.rail_network.rail_lines[0].id],
        },
        status: InfrastructureProjectStatus::Open,
        timeline: InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(1_000),
            review_started_at: Some(UtcSeconds::from_unix_seconds(1_100)),
            proposed_at: Some(UtcSeconds::from_unix_seconds(1_200)),
            approved_at: Some(UtcSeconds::from_unix_seconds(1_300)),
            funding_completed_at: Some(UtcSeconds::from_unix_seconds(1_400)),
            scheduled_start_at: Some(UtcSeconds::from_unix_seconds(1_500)),
            construction_started_at: Some(UtcSeconds::from_unix_seconds(1_600)),
            planned_completion_at: Some(UtcSeconds::from_unix_seconds(1_700)),
            completed_at: Some(UtcSeconds::from_unix_seconds(1_700)),
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        },
        funding: InfrastructureProjectFunding {
            estimated_cost: historical_commitment,
            authority_committed: historical_commitment,
            operator_contributed: Money::ZERO,
            access_fee_discount: None,
        },
    }];

    assert_eq!(validate_game_state(&state), Ok(()));
}

#[test]
fn validation_rejects_more_active_projects_than_construction_capacity() {
    let mut state = active_game();
    state.region.rail_authority.construction_capacity = 1;
    let timeline = InfrastructureProjectTimeline {
        requested_at: UtcSeconds::from_unix_seconds(2_000),
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
    state.region.rail_authority.infrastructure_projects = vec![
        InfrastructureProject {
            id: InfrastructureProjectId::new(101),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(1)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline: timeline.clone(),
            funding: InfrastructureProjectFunding::default(),
        },
        InfrastructureProject {
            id: InfrastructureProjectId::new(102),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![RailLineId::new(2)],
            },
            status: InfrastructureProjectStatus::Construction,
            timeline,
            funding: InfrastructureProjectFunding::default(),
        },
    ];

    assert_eq!(
        validate_game_state(&state),
        Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority active construction exceeds its capacity",
        })
    );
}

#[test]
fn save_uses_normalized_tables_instead_of_one_state_blob() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let slot = SaveSlot::open(&path).unwrap();
    slot.save(&active_game()).unwrap();

    let connection = Connection::open(path).unwrap();
    let train_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM trains", [], |row| row.get(0))
        .unwrap();
    let journey_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM active_journeys", [], |row| row.get(0))
        .unwrap();
    let service_stop_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM service_stops", [], |row| row.get(0))
        .unwrap();
    let passenger_group_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM journey_passenger_groups", [], |row| {
            row.get(0)
        })
        .unwrap();
    let state_blob_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='game_state'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let saved_catalogue_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='diesel_catalogue'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let train_id: String = connection
        .query_row("SELECT id FROM trains LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let station_id: String = connection
        .query_row("SELECT id FROM rail_stations LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let model_id: String = connection
        .query_row("SELECT model_id FROM trains LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let evn: String = connection
        .query_row("SELECT evn FROM trains LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let next_train_display_number: i64 = connection
        .query_row(
            "SELECT next_train_display_number FROM company WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let next_unit_number: i64 = connection
        .query_row(
            "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(train_count, 1);
    assert_eq!(journey_count, 1);
    assert_eq!(service_stop_count, 2);
    assert!(passenger_group_count > 0);
    assert_eq!(state_blob_table, 0);
    assert_eq!(saved_catalogue_table, 0);
    for id in [&train_id, &station_id] {
        uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }
    assert_eq!(model_id, "helvetra-r70");
    let evn = EuropeanVehicleNumber::parse(&evn).unwrap();
    assert_eq!(evn.registration_code(), 67);
    assert_eq!(evn.series_code(), 701);
    assert_eq!(evn.unit_number(), 1);
    assert_eq!(next_train_display_number, 2);
    assert_eq!(next_unit_number, 2);
}

#[test]
fn v9_catalogue_ids_migrate_to_replacement_models() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    {
        let slot = SaveSlot::open(&path).unwrap();
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(1_000_000);
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        purchase_train(&mut state, 1, RailStationId::new(1)).unwrap();
        slot.save(&state).unwrap();
    }

    let connection = Connection::open(&path).unwrap();
    let old_local_evn = EuropeanVehicleNumber::generate(95, 67, 70, 1).unwrap();
    let old_express_evn = EuropeanVehicleNumber::generate(95, 67, 120, 1).unwrap();
    connection
        .execute(
            "UPDATE trains SET model_id = 'local-70', evn = ?1 WHERE model_id = 'helvetra-r70'",
            params![old_local_evn.as_str()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE trains SET model_id = 'express-120', evn = ?1 WHERE model_id = 'veltrian-d121'",
            params![old_express_evn.as_str()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE train_model_sequences SET model_id = 'local-70' WHERE model_id = 'helvetra-r70'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE train_model_sequences SET model_id = 'express-120' WHERE model_id = 'veltrian-d121'",
            [],
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", 9_u32)
        .unwrap();

    // Exercise only the catalogue-ID migration here. The database was
    // created with the current schema, so running the entire historical
    // schema chain would intentionally try to re-add later columns.
    migrate_v9_to_v10(&connection, &path).unwrap();

    let migrated_models = query_all(
        &connection,
        "SELECT model_id, evn FROM trains ORDER BY evn",
        &path,
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap();
    assert_eq!(
        migrated_models
            .iter()
            .map(|(model_id, evn)| {
                (
                    model_id.as_str(),
                    EuropeanVehicleNumber::parse(evn).unwrap().series_code(),
                )
            })
            .collect::<Vec<_>>(),
        vec![("helvetra-r70", 701), ("veltrian-d121", 721)]
    );
    let local_next: i64 = connection
        .query_row(
            "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let express_next: i64 = connection
        .query_row(
            "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'veltrian-d121'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(local_next, 2);
    assert_eq!(express_next, 2);
}

#[test]
fn missing_database_is_reported_without_creating_a_fresh_game() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let slot = SaveSlot::open(&path).unwrap();

    assert_eq!(slot.load().unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn second_opener_cannot_own_the_same_save_slot() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let _first_slot = SaveSlot::open(&path).unwrap();

    assert!(matches!(
        SaveSlot::open(path),
        Err(SaveSlotError::AlreadyOwned { .. })
    ));
}

#[test]
fn unsupported_schema_version_is_preserved_and_rejected() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "user_version", SAVE_VERSION + 1)
        .unwrap();
    drop(connection);
    let slot = SaveSlot::open(&path).unwrap();

    assert!(matches!(
        slot.load(),
        Err(SaveSlotError::InvalidSave { .. })
    ));
    assert!(path.exists());
}

#[test]
fn restart_backup_preserves_the_old_database_before_replacing_it() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let slot = SaveSlot::open(&path).unwrap();
    let old_state = active_game();
    slot.save(&old_state).unwrap();
    let replacement = create_new_game(99, "New Passenger", UtcSeconds::from_unix_seconds(2));

    let backup_path = slot.save_after_backup(&replacement).unwrap();

    assert!(backup_path.exists());
    let backup_slot = SaveSlot::open(&backup_path).unwrap();
    assert_eq!(backup_slot.load().unwrap(), Some(old_state));
    assert_eq!(slot.load().unwrap(), Some(replacement));
}

#[test]
fn v1_sqlite_schema_migrates_owned_trains_to_stable_model_ids() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE game_meta (
                 singleton INTEGER PRIMARY KEY,
                 world_seed TEXT NOT NULL,
                 last_processed_at INTEGER NOT NULL
             );
             CREATE TABLE region (
                 singleton INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 population INTEGER NOT NULL,
                 rail_authority_name TEXT NOT NULL
             );
             INSERT INTO game_meta VALUES(1, '42', 0);
             INSERT INTO region VALUES(1, 'Federation of Varelia', 1000, 'Federation of Varelia Rail Authority');
             CREATE TABLE company (
                 singleton INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 funds_cents INTEGER NOT NULL
             );
             INSERT INTO company VALUES(1, 'One More Prime', 1000000);
             CREATE TABLE trains (
                 id INTEGER PRIMARY KEY,
                 status_kind TEXT NOT NULL,
                 status_ref_id TEXT NOT NULL,
                 model_name TEXT NOT NULL,
                 original_purchase_price_cents INTEGER NOT NULL,
                 passenger_capacity INTEGER NOT NULL,
                 speed_metres_per_second INTEGER NOT NULL,
                 fuel_cost_cents_per_km INTEGER NOT NULL
             );
             CREATE TABLE diesel_catalogue (
                 sequence INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 purchase_price_cents INTEGER NOT NULL,
                 passenger_capacity INTEGER NOT NULL,
                 speed_metres_per_second INTEGER NOT NULL,
                 fuel_cost_cents_per_km INTEGER NOT NULL
             );
             CREATE TABLE rail_stations (id INTEGER PRIMARY KEY);
             CREATE TABLE rail_lines (id INTEGER PRIMARY KEY);
             CREATE TABLE passenger_services (
                 id INTEGER PRIMARY KEY,
                 first_station_id INTEGER NOT NULL,
                 second_station_id INTEGER NOT NULL
             );
             CREATE TABLE service_lines (
                 service_id INTEGER NOT NULL,
                 sequence INTEGER NOT NULL,
                 rail_line_id INTEGER NOT NULL,
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE active_journeys (
                 id INTEGER PRIMARY KEY,
                 service_id INTEGER NOT NULL,
                 train_id INTEGER NOT NULL REFERENCES trains(id),
                 origin_station_id INTEGER NOT NULL,
                 destination_station_id INTEGER NOT NULL,
                 passengers_carried INTEGER NOT NULL,
                 fare_cents INTEGER NOT NULL,
                 operating_revenue_cents INTEGER NOT NULL,
                 infrastructure_access_fee_cents INTEGER NOT NULL,
                 fuel_cost_cents INTEGER NOT NULL,
                 departed_at INTEGER NOT NULL,
                 arrives_at INTEGER NOT NULL
             );
             INSERT INTO rail_stations VALUES(1);
             INSERT INTO rail_stations VALUES(2);
             INSERT INTO rail_lines VALUES(1);
             INSERT INTO passenger_services VALUES(1, 1, 2);
             INSERT INTO service_lines VALUES(1, 0, 1);
             INSERT INTO trains VALUES(1, 'travelling', 1, 'Local 70', 300000, 70, 25, 45);
             INSERT INTO diesel_catalogue VALUES(0, 'Local 70', 300000, 70, 25, 45);
             INSERT INTO active_journeys VALUES(1, 1, 1, 1, 2, 1, 100, 100, 10, 10, 0, 60);
             PRAGMA user_version = 1;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let migrated_train_id = "00000005-0000-4000-8000-000000000001";
    let migrated_journey_id = "00000007-0000-4000-8000-000000000001";
    let migrated_service_id = "00000006-0000-4000-8000-000000000001";
    let model_id: String = connection
        .query_row(
            "SELECT model_id FROM trains WHERE id = ?1",
            params![migrated_train_id],
            |row| row.get(0),
        )
        .unwrap();
    let evn: String = connection
        .query_row(
            "SELECT evn FROM trains WHERE id = ?1",
            params![migrated_train_id],
            |row| row.get(0),
        )
        .unwrap();
    let next_train_display_number: i64 = connection
        .query_row(
            "SELECT next_train_display_number FROM company WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let next_unit_number: i64 = connection
        .query_row(
            "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let old_catalogue_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='diesel_catalogue'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let journey_train_id: String = connection
        .query_row(
            "SELECT train_id FROM active_journeys WHERE id = ?1",
            params![migrated_journey_id],
            |row| row.get(0),
        )
        .unwrap();
    let service_name: String = connection
        .query_row(
            "SELECT name FROM passenger_services WHERE id = ?1",
            params![migrated_service_id],
            |row| row.get(0),
        )
        .unwrap();
    let service_stop_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM service_stops WHERE service_id = ?1",
            params![migrated_service_id],
            |row| row.get(0),
        )
        .unwrap();
    let foreign_key_violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    let (registration_code, registration_mark): (i64, String) = connection
        .query_row(
            "SELECT registration_code, registration_mark FROM region WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let company_vkm: String = connection
        .query_row("SELECT vkm FROM company WHERE singleton = 1", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(model_id, "helvetra-r70");
    assert_eq!(evn, "956707010016");
    assert_eq!(next_train_display_number, 2);
    assert_eq!(next_unit_number, 2);
    assert_eq!(old_catalogue_exists, 0);
    assert_eq!(journey_train_id, migrated_train_id);
    assert_eq!(service_name, "R1");
    assert_eq!(service_stop_count, 2);
    assert_eq!(foreign_key_violations, 0);
    assert_eq!(registration_code, 67);
    assert_eq!(registration_mark, "VA");
    assert_eq!(company_vkm, "OMP");
}

#[test]
fn v12_schema_migrates_rail_authority_finances_with_safe_defaults() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE region (
                 singleton INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 registration_code INTEGER NOT NULL,
                 registration_mark TEXT NOT NULL,
                 population INTEGER NOT NULL,
                 rail_authority_name TEXT NOT NULL,
                 next_infrastructure_project_id INTEGER NOT NULL
             );
             INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority', 1);
             PRAGMA user_version = 12;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let finances: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT treasury_cents, maintenance_reserve_cents, committed_investment_cents,
                    carried_over_funds_cents
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(
        finances,
        (
            crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents(),
            0,
            0,
            0
        )
    );
}

#[test]
fn v17_schema_adds_rail_authority_construction_capacity() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE region (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 name TEXT NOT NULL,
                 registration_code INTEGER NOT NULL,
                 registration_mark TEXT NOT NULL,
                 population INTEGER NOT NULL,
                 rail_authority_name TEXT NOT NULL
             );
             INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
             PRAGMA user_version = 17;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let capacity: i64 = connection
        .query_row(
            "SELECT rail_authority_construction_capacity FROM region WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(
        capacity,
        i64::from(crate::model::PROVISIONAL_CONSTRUCTION_CAPACITY)
    );
}

#[test]
fn v16_schema_migrates_entity_ids_to_uuid_v4_text() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(V16_IDENTITY_TABLES_COMPAT_SCHEMA)
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 2000, 'Varelia Rail Authority', 2);
             INSERT INTO settlements VALUES(1, 'Alden', 1000);
             INSERT INTO settlements VALUES(2, 'Bellhaven', 1000);
             INSERT INTO rail_stations VALUES(1, 1);
             INSERT INTO rail_stations VALUES(2, 2);
             INSERT INTO rail_lines VALUES(1, 1, 2, 10000, 70, 1, 'none', 'moderate');
             INSERT INTO infrastructure_projects(
                 id, kind, status, requested_at, target_speed_limit_kmh
             ) VALUES(1, 'speed_upgrade', 'requested', 1000, 100);
             INSERT INTO infrastructure_project_rail_lines VALUES(1, 0, 1);
             INSERT INTO company VALUES(1, 'One More Prime', 'OMP', 500000, 2);
             INSERT INTO trains VALUES(1, '956707010016', NULL, 'travelling', 1, 'helvetra-r70', 300000);
             INSERT INTO passenger_services VALUES(1, 'R1');
             INSERT INTO service_stops VALUES(1, 0, 1);
             INSERT INTO service_stops VALUES(1, 1, 2);
             INSERT INTO service_lines VALUES(1, 0, 1);
             INSERT INTO origin_destination_demand VALUES(1, 2, 10, 20, 0);
             INSERT INTO active_journeys(
                 id, service_id, train_id, origin_station_id, destination_station_id,
                 passengers_carried, fare_cents, operating_revenue_cents,
                 credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents,
                 current_stop_index, departed_at, arrives_at
             ) VALUES(1, 1, 1, 1, 2, 10, 1000, 1000, 0, 100, 50, 1, 1000, 1100);
             INSERT INTO journey_passenger_groups VALUES(1, 0, 1, 2, 10, 1000);
             INSERT INTO journey_receipts VALUES(1, 1000, 100, 50, 1, 'Helvetra R70', 1, 2, 10, 70, 1100);
             PRAGMA user_version = 16;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let train_id: String = connection
        .query_row("SELECT id FROM trains", [], |row| row.get(0))
        .unwrap();
    let model_id: String = connection
        .query_row("SELECT model_id FROM trains", [], |row| row.get(0))
        .unwrap();
    let line_id: String = connection
        .query_row("SELECT id FROM rail_lines", [], |row| row.get(0))
        .unwrap();
    let project_id: String = connection
        .query_row("SELECT id FROM infrastructure_projects", [], |row| {
            row.get(0)
        })
        .unwrap();
    let journey_train_id: String = connection
        .query_row("SELECT train_id FROM active_journeys", [], |row| row.get(0))
        .unwrap();
    let foreign_key_violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();

    for id in [&train_id, &line_id, &project_id] {
        uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }
    assert_eq!(version, SAVE_VERSION);
    assert_eq!(train_id, "00000005-0000-4000-8000-000000000001");
    assert_eq!(line_id, "00000003-0000-4000-8000-000000000001");
    assert_eq!(project_id, "00000004-0000-4000-8000-000000000001");
    assert_eq!(journey_train_id, train_id);
    assert_eq!(model_id, "helvetra-r70");
    assert_eq!(foreign_key_violations, 0);
}

#[test]
fn v22_schema_aligns_the_next_authority_fiscal_period_to_utc_midnight() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE rail_authority_finances (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                 maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                 committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                 carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                 regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
                 infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000, 0);
             CREATE TABLE game_meta (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 world_seed TEXT NOT NULL,
                 last_processed_at INTEGER NOT NULL
             );
             INSERT INTO game_meta VALUES(1, '42', 1000);
             PRAGMA user_version = 22;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let next_fiscal_period_at: i64 = connection
        .query_row(
            "SELECT next_fiscal_period_at FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(next_fiscal_period_at, 86_400);
}

#[test]
fn v23_schema_realigns_existing_fiscal_schedule_to_next_utc_midnight() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE rail_authority_finances (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                 maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                 committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                 carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                 regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
                 infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0,
                 next_fiscal_period_at INTEGER
             );
             INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000, 0, 22600);
             CREATE TABLE game_meta (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 world_seed TEXT NOT NULL,
                 last_processed_at INTEGER NOT NULL
             );
             INSERT INTO game_meta VALUES(1, '42', 50000);
             PRAGMA user_version = 23;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let next_fiscal_period_at: i64 = connection
        .query_row(
            "SELECT next_fiscal_period_at FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(next_fiscal_period_at, 86_400);
}

#[test]
fn v25_schema_backfills_project_reconsideration_count() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE infrastructure_projects (
                 id TEXT PRIMARY KEY
             );
             INSERT INTO infrastructure_projects(id) VALUES('project-1');
             PRAGMA user_version = 25;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let reconsideration_count: i64 = connection
        .query_row(
            "SELECT reconsideration_count FROM infrastructure_projects WHERE id = 'project-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(reconsideration_count, 0);
}

#[test]
fn v26_schema_adds_persistent_railway_bulletin() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA user_version = 26;")
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let bulletin_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'bulletin_entries'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(bulletin_table, 1);
}

#[test]
fn v24_schema_backfills_existing_markets_as_fully_mature() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE origin_destination_demand (
                 origin_station_id TEXT NOT NULL,
                 destination_station_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL UNIQUE,
                 waiting_passengers INTEGER NOT NULL,
                 passenger_arrival_rate_per_hour INTEGER NOT NULL,
                 fractional_passenger_seconds INTEGER NOT NULL,
                 PRIMARY KEY (origin_station_id, destination_station_id)
             );
             INSERT INTO origin_destination_demand VALUES('a', 'b', 0, 12, 4, 0);
             PRAGMA user_version = 24;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let maturity: i64 = connection
        .query_row(
            "SELECT market_maturity_basis_points FROM origin_destination_demand",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(maturity, 10_000);
}

#[test]
fn v15_schema_credits_historical_access_fees_to_authority() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE rail_authority_finances (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                 maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                 committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                 carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                 regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0)
             );
             INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000);
             CREATE TABLE financials (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 operating_revenue_cents INTEGER NOT NULL,
                 infrastructure_access_fees_cents INTEGER NOT NULL,
                 fuel_costs_cents INTEGER NOT NULL
             );
             INSERT INTO financials VALUES(1, 2000000, 325000, 175000);
             PRAGMA user_version = 15;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let (treasury, access_fee_revenue): (i64, i64) = connection
        .query_row(
            "SELECT treasury_cents, infrastructure_access_fee_revenue_cents
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(access_fee_revenue, 325_000);
    assert_eq!(treasury, 10_325_000);
}

#[test]
fn v14_schema_seeds_network_sized_maintenance_reserve() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE rail_authority_finances (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                 maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                 committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                 carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                 regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0)
             );
             INSERT INTO rail_authority_finances VALUES(1, 10000000, 0, 1000000, 0, 10000000);
             CREATE TABLE settlements (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 population INTEGER NOT NULL
             );
             INSERT INTO settlements VALUES(1, 'A', 1000);
             INSERT INTO settlements VALUES(2, 'B', 1000);
             INSERT INTO settlements VALUES(3, 'C', 1000);
             CREATE TABLE rail_stations (
                 id INTEGER PRIMARY KEY,
                 settlement_id INTEGER NOT NULL REFERENCES settlements(id)
             );
             INSERT INTO rail_stations VALUES(1, 1);
             INSERT INTO rail_stations VALUES(2, 2);
             INSERT INTO rail_stations VALUES(3, 3);
             CREATE TABLE rail_lines (
                 id INTEGER PRIMARY KEY,
                 first_station_id INTEGER NOT NULL,
                 second_station_id INTEGER NOT NULL,
                 distance_metres INTEGER NOT NULL,
                 speed_limit_kmh INTEGER NOT NULL,
                 track_count INTEGER NOT NULL,
                 electrification TEXT NOT NULL,
                 construction_difficulty TEXT NOT NULL
             );
             INSERT INTO rail_lines VALUES(1, 1, 2, 10000, 70, 1, 'none', 'moderate');
             INSERT INTO rail_lines VALUES(2, 2, 3, 5000, 70, 2, 'none', 'moderate');
             PRAGMA user_version = 14;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let reserve: i64 = connection
        .query_row(
            "SELECT maintenance_reserve_cents
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(reserve, 500_000);
}

#[test]
fn v13_schema_migrates_and_seeds_regional_public_allocation() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE rail_authority_finances (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                 maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                 committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                 carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0)
             );
             INSERT INTO rail_authority_finances VALUES(1, 5000, 1000, 2000, 500);
             PRAGMA user_version = 13;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let (treasury, allocation): (i64, i64) = connection
        .query_row(
            "SELECT treasury_cents, regional_public_allocation_cents
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(
        allocation,
        crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents()
    );
    assert_eq!(treasury, 5000 + allocation);
}

#[test]
fn v11_schema_migrates_infrastructure_project_persistence() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE region (
                 singleton INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 registration_code INTEGER NOT NULL,
                 registration_mark TEXT NOT NULL,
                 population INTEGER NOT NULL,
                 rail_authority_name TEXT NOT NULL
             );
             INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
             CREATE TABLE settlements (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 population INTEGER NOT NULL
             );
             CREATE TABLE rail_stations (
                 id INTEGER PRIMARY KEY,
                 settlement_id INTEGER NOT NULL REFERENCES settlements(id)
             );
             CREATE TABLE rail_lines (
                 id INTEGER PRIMARY KEY,
                 first_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 second_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 distance_metres INTEGER NOT NULL,
                 speed_limit_kmh INTEGER NOT NULL,
                 track_count INTEGER NOT NULL,
                 electrification TEXT NOT NULL,
                 construction_difficulty TEXT NOT NULL
             );
             PRAGMA user_version = 11;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let project_table_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'infrastructure_projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(project_table_exists, 1);
}

#[test]
fn v10_schema_migrates_rail_line_capabilities_with_safe_defaults() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE region (
                 singleton INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 registration_code INTEGER NOT NULL,
                 registration_mark TEXT NOT NULL,
                 population INTEGER NOT NULL,
                 rail_authority_name TEXT NOT NULL
             );
             INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
             CREATE TABLE settlements (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 population INTEGER NOT NULL
             );
             INSERT INTO settlements VALUES(1, 'A', 500);
             INSERT INTO settlements VALUES(2, 'B', 500);
             CREATE TABLE rail_stations (
                 id INTEGER PRIMARY KEY,
                 settlement_id INTEGER NOT NULL REFERENCES settlements(id)
             );
             INSERT INTO rail_stations VALUES(1, 1);
             INSERT INTO rail_stations VALUES(2, 2);
             CREATE TABLE rail_lines (
                 id INTEGER PRIMARY KEY,
                 first_station_id INTEGER NOT NULL,
                 second_station_id INTEGER NOT NULL,
                 distance_metres INTEGER NOT NULL
             );
             INSERT INTO rail_lines VALUES(1, 1, 2, 42000);
             PRAGMA user_version = 10;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let capabilities: (i64, i64, String, String) = connection
        .query_row(
            "SELECT speed_limit_kmh, track_count, electrification, construction_difficulty
             FROM rail_lines WHERE id = '00000003-0000-4000-8000-000000000001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(capabilities, (70, 1, "none".into(), "moderate".into()));
}

#[test]
fn legacy_ron_decoder_accepts_the_previous_versioned_envelope() {
    let state = active_game();
    let source = legacy::encode_v1_for_test(&state);

    assert_eq!(decode_legacy_game_state(&source).unwrap(), state);
}

fn create_v28_passenger_service_migration_schema(connection: &Connection) {
    connection
        .execute_batch(
            "CREATE TABLE passenger_services (
                 id TEXT PRIMARY KEY,
                 sequence INTEGER NOT NULL UNIQUE,
                 name TEXT NOT NULL,
                 direction_mode TEXT NOT NULL DEFAULT 'both'
                     CHECK (direction_mode IN ('both', 'forward'))
             );
             CREATE TABLE service_stops (
                 service_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 station_id TEXT NOT NULL,
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE service_lines (
                 service_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 rail_line_id TEXT NOT NULL,
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE rail_lines (
                 id TEXT PRIMARY KEY,
                 first_station_id TEXT NOT NULL,
                 second_station_id TEXT NOT NULL
             );
             CREATE TABLE active_journeys (
                 id TEXT PRIMARY KEY,
                 service_id TEXT NOT NULL
             );
             PRAGMA user_version = 28;",
        )
        .unwrap();
}

fn insert_v28_reciprocal_service_pair(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO passenger_services(id, sequence, name) VALUES
                 ('service-a', 0, 'R10'),
                 ('service-b', 1, 'R11');
             INSERT INTO rail_lines(id, first_station_id, second_station_id) VALUES
                 ('line-ab', 'station-a', 'station-b'),
                 ('line-bc', 'station-b', 'station-c');
             INSERT INTO service_stops(service_id, sequence, station_id) VALUES
                 ('service-a', 0, 'station-a'),
                 ('service-a', 1, 'station-b'),
                 ('service-a', 2, 'station-c'),
                 ('service-b', 0, 'station-c'),
                 ('service-b', 1, 'station-b'),
                 ('service-b', 2, 'station-a');
             INSERT INTO service_lines(service_id, sequence, rail_line_id) VALUES
                 ('service-a', 0, 'line-ab'),
                 ('service-a', 1, 'line-bc'),
                 ('service-b', 0, 'line-bc'),
                 ('service-b', 1, 'line-ab');",
        )
        .unwrap();
}

#[test]
fn v27_schema_falls_back_to_one_way_when_route_cannot_be_verified() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE passenger_services (
                 id TEXT PRIMARY KEY,
                 sequence INTEGER NOT NULL UNIQUE,
                 name TEXT NOT NULL
             );
             INSERT INTO passenger_services(id, sequence, name)
             VALUES('00000004-0000-4000-8000-000000000001', 0, 'R1');
             PRAGMA user_version = 27;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let direction_mode: String = connection
        .query_row(
            "SELECT direction_mode FROM passenger_services WHERE sequence = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(direction_mode, "forward");
}

#[test]
fn v28_migration_merges_idle_reciprocal_services_into_the_older_service() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    create_v28_passenger_service_migration_schema(&connection);
    insert_v28_reciprocal_service_pair(&connection);

    ensure_schema(&connection, &path).unwrap();

    let remaining: (String, String) = connection
        .query_row(
            "SELECT id, direction_mode FROM passenger_services",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let service_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM passenger_services", [], |row| {
            row.get(0)
        })
        .unwrap();
    let retired_stop_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM service_stops WHERE service_id = 'service-b'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(service_count, 1);
    assert_eq!(remaining, ("service-a".into(), "both".into()));
    assert_eq!(retired_stop_count, 0);
}

#[test]
fn v28_migration_preserves_the_active_direction_when_merging_a_pair() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    create_v28_passenger_service_migration_schema(&connection);
    insert_v28_reciprocal_service_pair(&connection);
    connection
        .execute(
            "INSERT INTO active_journeys(id, service_id) VALUES('journey-1', 'service-b')",
            [],
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let remaining: (String, String) = connection
        .query_row(
            "SELECT id, direction_mode FROM passenger_services",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let active_service_id: String = connection
        .query_row("SELECT service_id FROM active_journeys", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert_eq!(remaining, ("service-b".into(), "both".into()));
    assert_eq!(active_service_id, "service-b");
}

#[test]
fn v28_migration_keeps_both_reciprocal_services_one_way_when_both_are_active() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    create_v28_passenger_service_migration_schema(&connection);
    insert_v28_reciprocal_service_pair(&connection);
    connection
        .execute_batch(
            "INSERT INTO active_journeys(id, service_id) VALUES
                 ('journey-1', 'service-a'),
                 ('journey-2', 'service-b');",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let service_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM passenger_services", [], |row| {
            row.get(0)
        })
        .unwrap();
    let bidirectional_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM passenger_services WHERE direction_mode = 'both'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(service_count, 2);
    assert_eq!(bidirectional_count, 0);
}

#[test]
fn v28_migration_upgrades_a_valid_standalone_service_to_bidirectional() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    create_v28_passenger_service_migration_schema(&connection);
    connection
        .execute_batch(
            "INSERT INTO passenger_services(id, sequence, name) VALUES('service-a', 0, 'R10');
             INSERT INTO rail_lines(id, first_station_id, second_station_id)
             VALUES('line-ab', 'station-a', 'station-b');
             INSERT INTO service_stops(service_id, sequence, station_id) VALUES
                 ('service-a', 0, 'station-a'),
                 ('service-a', 1, 'station-b');
             INSERT INTO service_lines(service_id, sequence, rail_line_id)
             VALUES('service-a', 0, 'line-ab');",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let service: (String, i64, Option<i64>) = connection
        .query_row(
            "SELECT direction_mode, forward_train_number, reverse_train_number
             FROM passenger_services WHERE id = 'service-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(service, ("both".into(), 100, Some(101)));
}

#[test]
fn v33_migration_adds_the_slower_authority_pacing_target() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE game_rules (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 fare_cents_per_passenger_km INTEGER NOT NULL,
                 access_fee_cents_per_train_km INTEGER NOT NULL,
                 starting_company_funds_cents INTEGER NOT NULL,
                 demand_cap_seconds INTEGER NOT NULL
             );
             INSERT INTO game_rules(
                 singleton, fare_cents_per_passenger_km, access_fee_cents_per_train_km,
                 starting_company_funds_cents, demand_cap_seconds
             ) VALUES(1, 20, 12, 500000, 86400);
             PRAGMA user_version = 33;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let rules: (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT authority_request_queue_seconds, authority_review_seconds,
                    authority_proposal_seconds, authority_request_cooldown_seconds,
                    authority_deferred_reconsideration_seconds, authority_mobilisation_seconds,
                    authority_new_line_base_construction_seconds,
                    authority_low_difficulty_seconds_per_km,
                    authority_moderate_difficulty_seconds_per_km,
                    authority_high_difficulty_seconds_per_km,
                    authority_max_active_expansion_projects
             FROM game_rules WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(
        rules,
        (
            3_600, 7_200, 3_600, 86_400, 86_400, 3_600, 18_000, 120, 180, 240, 2
        )
    );
}

#[test]
fn v34_migration_adds_rejected_infrastructure_project_status() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    let v34_schema = SCHEMA.replace(
        "'approved', 'deferred', 'rejected', 'funding'",
        "'approved', 'deferred', 'funding'",
    );
    assert!(!v34_schema.contains("'rejected'"));
    connection.execute_batch(&v34_schema).unwrap();
    connection
        .execute(
            "INSERT INTO infrastructure_projects(id, sequence, kind, status, requested_at)
             VALUES('00000004-0000-4000-8000-000000000001', 0, 'new_line', 'deferred', 1000)",
            [],
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", 34_u32)
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let status: String = connection
        .query_row(
            "SELECT status FROM infrastructure_projects WHERE sequence = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let table_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'infrastructure_projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(status, "deferred");
    assert!(table_sql.contains("'rejected'"));
}

#[test]
fn v35_migration_rebalances_default_operating_rates_and_preserves_custom_rates() {
    for (fare, access, expected_fare, expected_access) in [(20, 12, 12, 35), (18, 9, 18, 9)] {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO game_rules(
                     singleton, fare_cents_per_passenger_km, access_fee_cents_per_train_km,
                     starting_company_funds_cents, demand_cap_seconds,
                     authority_request_queue_seconds, authority_review_seconds,
                     authority_proposal_seconds, authority_request_cooldown_seconds,
                     authority_deferred_reconsideration_seconds, authority_mobilisation_seconds,
                     authority_new_line_base_construction_seconds,
                     authority_low_difficulty_seconds_per_km,
                     authority_moderate_difficulty_seconds_per_km,
                     authority_high_difficulty_seconds_per_km,
                     authority_max_active_expansion_projects
                 ) VALUES(1, ?1, ?2, 500000, 86400, 3600, 7200, 3600, 86400, 86400,
                          3600, 18000, 120, 180, 240, 2)",
                params![fare, access],
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", 35_u32)
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let rates: (i64, i64) = connection
            .query_row(
                "SELECT fare_cents_per_passenger_km, access_fee_cents_per_train_km
                 FROM game_rules WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(rates, (expected_fare, expected_access));
    }
}

#[test]
fn v36_migration_preserves_in_flight_legacy_fare_snapshot() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    let v36_schema = SCHEMA.replace(
        "    fare_rate_cents_per_passenger_km INTEGER NOT NULL CHECK (fare_rate_cents_per_passenger_km > 0),\n",
        "",
    );
    connection.execute_batch(&v36_schema).unwrap();
    connection
        .execute_batch(
            "INSERT INTO game_rules(
                 singleton, fare_cents_per_passenger_km, access_fee_cents_per_train_km,
                 starting_company_funds_cents, demand_cap_seconds,
                 authority_request_queue_seconds, authority_review_seconds,
                 authority_proposal_seconds, authority_request_cooldown_seconds,
                 authority_deferred_reconsideration_seconds, authority_mobilisation_seconds,
                 authority_new_line_base_construction_seconds,
                 authority_low_difficulty_seconds_per_km,
                 authority_moderate_difficulty_seconds_per_km,
                 authority_high_difficulty_seconds_per_km,
                 authority_max_active_expansion_projects
             ) VALUES(1, 12, 35, 500000, 86400, 3600, 7200, 3600, 86400, 86400,
                      3600, 18000, 120, 180, 240, 2);
             INSERT INTO passenger_services(
                 id, sequence, name, direction_mode, forward_train_number, reverse_train_number
             ) VALUES('00000006-0000-4000-8000-000000000001', 0, 'R1', 'both', 100, 101);
             INSERT INTO rail_lines(
                 id, sequence, first_station_id, second_station_id, distance_metres,
                 speed_limit_kmh, track_count, electrification, construction_difficulty
             ) VALUES(
                 '00000003-0000-4000-8000-000000000001', 0,
                 '00000002-0000-4000-8000-000000000001',
                 '00000002-0000-4000-8000-000000000002',
                 98000, 70, 1, 'none', 'moderate'
             );
             INSERT INTO service_lines(service_id, sequence, rail_line_id)
             VALUES(
                 '00000006-0000-4000-8000-000000000001', 0,
                 '00000003-0000-4000-8000-000000000001'
             );
             INSERT INTO active_journeys(
                 id, sequence, purpose, service_id, train_id, origin_station_id,
                 destination_station_id, passengers_carried, fare_cents,
                 operating_revenue_cents, credited_revenue_cents,
                 infrastructure_access_fee_cents, fuel_cost_cents,
                 current_stop_index, departed_at, arrives_at
             ) VALUES(
                 '00000007-0000-4000-8000-000000000001', 0, 'revenue',
                 '00000006-0000-4000-8000-000000000001',
                 '00000005-0000-4000-8000-000000000001',
                 '00000002-0000-4000-8000-000000000001',
                 '00000002-0000-4000-8000-000000000002',
                 10, 1960, 19600, 0, 1176, 3724, 0, 1000, 2000
             );
             PRAGMA user_version = 36;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let snapshot_rate: i64 = connection
        .query_row(
            "SELECT fare_rate_cents_per_passenger_km FROM active_journeys WHERE sequence = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(snapshot_rate, 20);
}

#[test]
fn v37_migration_converts_remaining_access_credit_into_temporary_discount() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    let v37_schema = SCHEMA
        .replace(
            "    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),\n",
            "    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),\n    access_fee_credit_awarded_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_awarded_cents >= 0),\n    access_fee_credit_remaining_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_remaining_cents >= 0),\n",
        )
        .replace(
            "    access_fee_discount_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_discount_basis_points BETWEEN 0 AND 10000),\n",
            "",
        )
        .replace("    access_fee_discount_expires_at INTEGER,\n", "");
    connection.execute_batch(&v37_schema).unwrap();
    connection
        .execute_batch(
            "INSERT INTO game_meta(singleton, world_seed, last_processed_at)
             VALUES(1, '42', 100000);
             INSERT INTO infrastructure_projects(
                 id, sequence, kind, status, estimated_cost_cents, authority_committed_cents,
                 operator_contributed_cents, access_fee_credit_awarded_cents,
                 access_fee_credit_remaining_cents, requested_at, completed_at
             ) VALUES(
                 '00000004-0000-4000-8000-000000000001', 0, 'renewal', 'open',
                 1000, 900, 100, 115, 60, 1000, 5000
             );
             PRAGMA user_version = 37;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let discount: (i64, Option<i64>) = connection
        .query_row(
            "SELECT access_fee_discount_basis_points, access_fee_discount_expires_at
             FROM infrastructure_projects WHERE sequence = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(discount, (5_000, Some(8 * 86_400)));
}

#[test]
fn v38_migration_removes_legacy_access_credit_columns_without_losing_discount() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    let v38_schema = SCHEMA.replace(
        "    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),\n",
        "    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),\n    access_fee_credit_awarded_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_awarded_cents >= 0),\n    access_fee_credit_remaining_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_remaining_cents >= 0),\n",
    );
    connection.execute_batch(&v38_schema).unwrap();
    connection
        .execute_batch(
            "INSERT INTO game_meta(singleton, world_seed, last_processed_at)
             VALUES(1, '42', 100000);
             INSERT INTO infrastructure_projects(
                 id, sequence, kind, status, estimated_cost_cents, authority_committed_cents,
                 operator_contributed_cents, access_fee_credit_awarded_cents,
                 access_fee_credit_remaining_cents, access_fee_discount_basis_points,
                 access_fee_discount_expires_at, requested_at, completed_at
             ) VALUES(
                 '00000004-0000-4000-8000-000000000001', 0, 'renewal', 'open',
                 1000, 900, 100, 115, 60, 5000, 691200, 1000, 5000
             );
             PRAGMA user_version = 38;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let discount: (i64, Option<i64>) = connection
        .query_row(
            "SELECT access_fee_discount_basis_points, access_fee_discount_expires_at
             FROM infrastructure_projects WHERE sequence = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(infrastructure_projects)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(discount, (5_000, Some(691_200)));
    assert!(
        !columns
            .iter()
            .any(|column| column == "access_fee_credit_awarded_cents")
    );
    assert!(
        !columns
            .iter()
            .any(|column| column == "access_fee_credit_remaining_cents")
    );
}

#[test]
fn v39_migration_adds_journey_service_telemetry_without_inventing_history() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE active_journeys (
                 id TEXT PRIMARY KEY,
                 departed_at INTEGER NOT NULL,
                 arrives_at INTEGER NOT NULL
             );
             CREATE TABLE journey_receipts (
                 journey_id TEXT PRIMARY KEY,
                 revenue_cents INTEGER NOT NULL,
                 infrastructure_access_fee_cents INTEGER NOT NULL,
                 fuel_cost_cents INTEGER NOT NULL,
                 completed_at INTEGER
             );
             INSERT INTO active_journeys VALUES('00000007-0000-4000-8000-000000000001', 1000, 2000);
             INSERT INTO journey_receipts VALUES('00000007-0000-4000-8000-000000000002', 5000, 400, 600, 3000);
             PRAGMA user_version = 39;",
        )
        .unwrap();

    ensure_schema(&connection, &path).unwrap();

    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let active_started_at: Option<i64> = connection
        .query_row(
            "SELECT started_at FROM active_journeys LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let receipt_telemetry: (Option<String>, Option<String>, Option<String>, Option<i64>) = connection
        .query_row(
            "SELECT service_id, service_code, purpose, departed_at FROM journey_receipts LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(version, SAVE_VERSION);
    assert_eq!(active_started_at, None);
    assert_eq!(receipt_telemetry, (None, None, None, None));
}

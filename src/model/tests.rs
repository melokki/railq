use std::{any::TypeId, collections::HashSet};

use crate::balance::BalanceConfig;

use super::vehicle::evn_check_digit;
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
            vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name("Alden Passenger"),
            funds: Money::from_cents(10_000),
            fleet: Fleet {
                service_assignments: Default::default(),
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
            authority: AuthorityRules::provisional(),
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

#[test]
fn operator_contribution_activates_half_price_access_for_seven_fiscal_days() {
    let opened_at = UtcSeconds::from_unix_seconds(90_000);
    let mut funding = InfrastructureProjectFunding {
        estimated_cost: Money::from_cents(1_000),
        authority_committed: Money::from_cents(900),
        operator_contributed: Money::from_cents(100),
        ..InfrastructureProjectFunding::default()
    };

    let discount = funding
        .activate_operator_access_discount(opened_at)
        .unwrap()
        .unwrap();

    assert_eq!(
        discount.basis_points,
        PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS
    );
    assert_eq!(discount.expires_at.unix_seconds(), 8 * 86_400);
    assert!(discount.is_active_at(UtcSeconds::from_unix_seconds(8 * 86_400 - 1)));
    assert!(!discount.is_active_at(UtcSeconds::from_unix_seconds(8 * 86_400)));
    assert_eq!(funding.access_fee_discount, Some(discount));
}

#[test]
fn project_without_operator_contribution_does_not_receive_access_discount() {
    let mut funding = InfrastructureProjectFunding::default();

    let discount = funding
        .activate_operator_access_discount(UtcSeconds::from_unix_seconds(90_000))
        .unwrap();

    assert_eq!(discount, None);
    assert_eq!(funding.access_fee_discount, None);
}

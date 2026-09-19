//! Task 25 coverage for useful, state-specific empty and idle prompts.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    ui::{Shell, ShellAction, capture_rendered_buffer_mut},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/25";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

#[test]
fn empty_fleet_only_points_to_buy_when_a_catalogue_train_is_affordable() {
    let state = create_new_game(42, "Empty Fleet", STARTED_AT);
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    let affordable = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(affordable.contains("No rolling stock"));
    assert!(affordable.contains("o==[##]==o"));
    assert!(affordable.contains("[3] Open Market"));
    assert!(affordable.contains("Purchase your first passenger Train"));
}

#[test]
fn empty_fleet_without_a_delivery_station_points_to_authority() {
    let mut state = create_new_game(42, "Blocked Fleet", STARTED_AT);
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .clear();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("No rolling stock"));
    assert!(rendered.contains("Authority must open a Rail Station"));
    assert!(rendered.contains("[5] Authority"));
    assert!(!rendered.contains("[3] Open Market"));
}

#[test]
fn all_travelling_fleet_shows_nearest_arrival_and_empty_queue_explains_dispatch()
-> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Idle Fleet", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1))?;
    let service_id =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))?;
    dispatch_journey(&mut state, train_id, service_id, STARTED_AT)?;

    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    let fleet = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(fleet.contains("FLEET IN SERVICE"), "{fleet}");
    assert!(fleet.contains("next arrival in"));
    assert!(fleet.contains("ETA"));
    fs::create_dir_all(EVIDENCE_DIR)?;
    fs::write(
        Path::new(EVIDENCE_DIR).join("all-travelling-fleet-120x40.txt"),
        &fleet,
    )?;

    let arrives_at = state.active_journeys[0].arrives_at;
    railq::sim::time::advance_time(&mut state, arrives_at)?;
    press(&mut shell, &state, KeyCode::Char('1'));
    press(&mut shell, &state, KeyCode::Tab);
    press(&mut shell, &state, KeyCode::Tab);
    let queue = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(queue.contains("Departure Board · active Journeys"));
    assert!(queue.contains("Dispatch a READY Train"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("empty-journey-queue-80x24.txt"),
        &queue,
    )?;
    Ok(())
}

#[test]
fn empty_history_and_zero_passenger_dispatch_are_explained_without_profit_promises()
-> Result<(), Box<dyn Error>> {
    fs::create_dir_all(EVIDENCE_DIR)?;
    let mut state = create_new_game(42, "Empty History", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    let _train_id = purchase_train(&mut state, 0, RailStationId::new(1))?;
    let _service_id =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))?;
    state
        .origin_destination_demand
        .iter_mut()
        .filter(|pool| {
            pool.origin_station_id == RailStationId::new(1)
                && pool.destination_station_id == RailStationId::new(2)
        })
        .for_each(|pool| pool.waiting_passengers = 0);

    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('4'));
    let history = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(history.contains("No retained Journey receipts yet"));
    assert!(
        history.contains("Operating Revenue is credited"),
        "{history}"
    );
    fs::write(
        Path::new(EVIDENCE_DIR).join("empty-history-120x40.txt"),
        &history,
    )?;

    press(&mut shell, &state, KeyCode::Char('1'));
    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    press(&mut shell, &state, KeyCode::Enter);
    let dispatch = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(dispatch.contains("zero passengers is valid"));
    assert!(!dispatch.to_lowercase().contains("guaranteed profit"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("zero-passenger-dispatch-120x40.txt"),
        &dispatch,
    )?;
    Ok(())
}

//! Keyboard and rendered-buffer coverage for the task 03 Fleet browser.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, TrainStatus, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        time::advance_time, world::create_new_game,
    },
    ui::{Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/03";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

fn operating_fleet() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    let travelling_train = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    for _ in 0..15 {
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    }
    let service =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2)).unwrap();
    dispatch_journey(&mut state, travelling_train, service, STARTED_AT).unwrap();
    state
}

#[test]
fn fleet_selection_is_keyboard_scrollable_and_survives_live_updates() {
    let mut state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('t'));

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Train"));
    assert!(wide.contains("Model"));
    assert!(wide.contains("Station / destination"));
    assert!(wide.contains("TRAVELLING"));
    assert!(wide.contains("in "));

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact.contains("> Train 01  TRAVELLING"));

    press(&mut shell, &state, KeyCode::Down);
    let down = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(down.contains("> Train 02  READY"));
    press(&mut shell, &state, KeyCode::Char('j'));
    let j = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(j.contains("> Train 03  READY"));
    press(&mut shell, &state, KeyCode::Char('k'));
    let k = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(k.contains("> Train 02  READY"));

    press(&mut shell, &state, KeyCode::PageDown);
    let page_down = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(page_down.contains("> Train 06  READY"));
    press(&mut shell, &state, KeyCode::PageDown);
    let beyond_viewport = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(beyond_viewport.contains("> Train 10  READY"));
    assert!(!beyond_viewport.contains("> Train 01"));
    press(&mut shell, &state, KeyCode::PageUp);
    let page_up = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(page_up.contains("> Train 06  READY"));

    let arrives_at = state.active_journeys[0].arrives_at;
    advance_time(&mut state, arrives_at).unwrap();
    let after_arrival = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(after_arrival.contains("> Train 06  READY"));

    state.player_company.fleet.trains.reverse();
    let after_reorder = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(after_reorder.contains("> Train 06  READY"));
}

#[test]
fn fleet_browser_states_missing_details_explicitly_and_captures_task_evidence()
-> Result<(), Box<dyn Error>> {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('t'));
    for _ in 0..9 {
        press(&mut shell, &state, KeyCode::Down);
    }

    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    for (columns, rows, file_name) in [
        (120, 40, "fleet-selected-120x40.txt"),
        (80, 24, "fleet-selected-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("> Train 10"));
        fs::write(evidence_dir.join(file_name), rendered)?;
    }

    let mut missing = create_new_game(42, "Missing-data Passenger", STARTED_AT);
    missing.player_company.funds = Money::from_cents(1_000_000);
    purchase_train(&mut missing, 0, RailStationId::new(1))?;
    missing.player_company.fleet.trains[0].model_name.clear();
    missing.player_company.fleet.trains[0].status = TrainStatus::Ready {
        at: RailStationId::new(99),
    };
    let mut missing_shell = Shell::new();
    press(&mut missing_shell, &missing, KeyCode::Char('t'));
    let missing_render = capture_rendered_buffer(&missing_shell, &missing, 120, 40);
    assert!(missing_render.contains("Model unavailable"));
    assert!(missing_render.contains("Missing Rail Station 99"));

    let empty = create_new_game(42, "Empty Fleet Passenger", STARTED_AT);
    let mut empty_shell = Shell::new();
    press(&mut empty_shell, &empty, KeyCode::Char('t'));
    let empty_render = capture_rendered_buffer(&empty_shell, &empty, 80, 24);
    assert!(empty_render.contains("No Trains in the Fleet"));
    Ok(())
}

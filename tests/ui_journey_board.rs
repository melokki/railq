//! Rendered-buffer coverage for the Map departure board and live arrivals.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, TrainStatus, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        time::advance_time, world::create_new_game,
    },
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/12";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

fn state_with_active_journey() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).expect("purchase train");
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("purchase backup train");
    let service_id =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
            .expect("create service");
    dispatch_journey(&mut state, train_id, service_id, STARTED_AT).expect("dispatch journey");
    state
}

fn open_departure_board(shell: &mut Shell, state: &railq::model::GameState) {
    press(shell, state, KeyCode::Tab);
    press(shell, state, KeyCode::Tab);
}

#[test]
fn departure_board_shows_selected_journey_progress_in_wide_and_compact_layouts()
-> Result<(), Box<dyn Error>> {
    let mut state = state_with_active_journey();
    let journey = state.active_journeys[0].clone();
    let halfway = UtcSeconds::from_unix_seconds(
        journey.departed_at.unix_seconds()
            + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
    );
    advance_time(&mut state, halfway)?;
    let mut shell = Shell::new();
    open_departure_board(&mut shell, &state);
    fs::create_dir_all(EVIDENCE_DIR)?;

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Departure Board · active Journeys"));
    assert!(wide.contains("Train"));
    assert!(wide.contains("Origin"));
    assert!(wide.contains("Next stop"));
    assert!(wide.contains("ETA"));
    assert!(wide.contains("Train 01"));
    assert!(wide.contains("Leg progress"));
    assert!(wide.contains("50%"));
    assert!(wide.contains("Map marker follows the current Service leg."));
    assert!(wide.contains("Real-time Journeys continue"));
    assert!(wide.contains("after exit."));
    let (selected_row, selected_column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Train 01").map(|column| (row, column)))
        .expect("selected Journey row must be visible");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(selected_column)?,
            u16::try_from(selected_row)?,
        ),
        Some((theme::BACKGROUND, theme::ACCENT)),
    );
    fs::write(
        Path::new(EVIDENCE_DIR).join("departure-board-120x40.txt"),
        &wide,
    )?;

    let compact_board = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_board.contains("Departure Board · active Journeys"));
    assert!(compact_board.contains("Train 01  ETA in"));
    press(&mut shell, &state, KeyCode::Enter);
    let compact_details = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_details.contains("Selected Journey"));
    assert!(compact_details.contains("Leg progress"));
    assert!(compact_details.contains("50%"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("departure-board-80x24.txt"),
        compact_details,
    )?;
    Ok(())
}

#[test]
fn reconciliation_removes_an_arrived_journey_and_keeps_its_ready_train_selected_in_fleet() {
    let mut state = state_with_active_journey();
    let train_id = state.active_journeys[0].train_id;
    let arrives_at = state.active_journeys[0].arrives_at;
    let mut shell = Shell::new();
    open_departure_board(&mut shell, &state);

    let before_arrival = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(before_arrival.contains("Train 01"));
    advance_time(&mut state, arrives_at).expect("reconciliation settles the due Journey");
    assert_eq!(state.last_processed_at, arrives_at);
    assert!(state.active_journeys.is_empty());
    assert_eq!(
        state.player_company.fleet.trains[0].status,
        TrainStatus::Ready {
            at: RailStationId::new(2)
        }
    );

    let after_arrival = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(after_arrival.contains("No active Journeys"));
    press(&mut shell, &state, KeyCode::Char('t'));
    let fleet = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(fleet.contains("› Train 01"));
    assert!(fleet.contains("READY"));
    assert_eq!(train_id.get(), 1);
}

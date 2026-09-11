//! Rendered-buffer and keyboard coverage for the Manual Dispatch Train chooser.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, TrainStatus, UtcSeconds},
    sim::{fleet::purchase_train, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/13";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(key(code), state)
}

fn state_with_ready_trains() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("first Train is purchased");
    purchase_train(&mut state, 0, RailStationId::new(2)).expect("second Train is purchased");
    state
}

#[test]
fn map_dispatch_prefers_the_focused_station_and_renders_a_stateful_ready_train_chooser()
-> Result<(), Box<dyn Error>> {
    let state = state_with_ready_trains();
    let before = state.clone();
    let mut shell = Shell::new();
    fs::create_dir_all(EVIDENCE_DIR)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("1 Train → 2 Destination → 3 Review"));
    assert!(wide.contains("Manual Dispatch · available Fleet"));
    assert!(wide.contains("Train 01"));
    assert!(wide.contains("Train 02"));
    assert!(wide.contains("READY"));
    assert!(wide.contains("Location"));
    assert!(wide.contains("a READY Train here is preselected"));
    let (row, column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Train 01").map(|column| (row, column)))
        .expect("focused Rail Station's Train is visibly selected");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column).expect("test terminal fits u16"),
            u16::try_from(row).expect("test terminal fits u16"),
        ),
        Some((theme::BACKGROUND, theme::ACCENT)),
    );
    fs::write(
        Path::new(EVIDENCE_DIR).join("train-chooser-120x40.txt"),
        &wide,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(!compact.contains("No READY Train at"));
    assert!(compact.contains("> Train 02"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("train-chooser-80x24.txt"),
        compact,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let destination = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(destination.contains("Select a destination for Train 2"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let fallback = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(fallback.contains("No READY Train at"));
    assert!(fallback.contains("Train 01"));
    assert!(fallback.contains("At Pinewatch"));
    assert!(fallback.contains("Train 02"));
    assert!(fallback.contains("At Oakridge"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        state, before,
        "Train selection and cancellation are presentation-only"
    );
    Ok(())
}

#[test]
fn chooser_pages_through_a_long_ready_fleet() {
    let mut state = create_new_game(42, "Long Fleet", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    for _ in 0..12 {
        purchase_train(&mut state, 0, RailStationId::new(1)).expect("Train is purchased");
    }
    let before = state.clone();
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let initial = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(initial.contains("> Train 01"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let paged = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(paged.contains("> Train 11"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(state, before, "scrolling is presentation-only");
}

#[test]
fn chooser_never_substitutes_a_changed_train_id_and_empty_fleet_explains_the_next_action() {
    let mut state = state_with_ready_trains();
    let mut shell = Shell::new();
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );

    let first_train_id = state.player_company.fleet.trains[0].id;
    let second_train_id = state.player_company.fleet.trains[1].id;
    state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
        journey_id: railq::model::JourneyId::new(99),
    };
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let changed = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(changed.contains("previously selected Train is no longer READY"));
    assert!(
        !changed.contains("> Train 02"),
        "a different Train is not silently selected"
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::ManualDispatch {
            train_id: second_train_id,
            destination_station_id: RailStationId::new(1),
        }
    );
    assert_ne!(first_train_id, second_train_id);

    let empty = create_new_game(42, "Empty Fleet", STARTED_AT);
    let mut empty_shell = Shell::new();
    assert_eq!(
        press(&mut empty_shell, &empty, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer(&empty_shell, &empty, 120, 40);
    assert!(rendered.contains("No READY Train in the Fleet. Press B to buy a Train."));
}

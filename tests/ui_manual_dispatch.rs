//! Rendered-buffer and keyboard coverage for Service-based Manual Dispatch.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, ServiceId, TrainStatus, UtcSeconds},
    sim::{fleet::purchase_train, services::create_service, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(key(code), state)
}

fn state_with_ready_trains_and_services() -> (railq::model::GameState, ServiceId, ServiceId) {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("first Train is purchased");
    purchase_train(&mut state, 0, RailStationId::new(2)).expect("second Train is purchased");
    let outbound = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(3)],
    )
    .expect("outbound Service is created");
    let second_origin = create_service(
        &mut state,
        vec![RailStationId::new(2), RailStationId::new(1)],
    )
    .expect("second-origin Service is created");
    (state, outbound, second_origin)
}

#[test]
fn manual_dispatch_moves_from_train_to_service_to_review() {
    let (state, outbound, _) = state_with_ready_trains_and_services();
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let train_step = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(train_step.contains("1 TRAIN"));
    assert!(train_step.contains("2 SERVICE"));
    assert!(train_step.contains("Choose Train"));
    assert!(train_step.contains("Train 01"));
    assert!(train_step.contains("Train 02"));
    let selected_train = &state.player_company.fleet.trains[0];
    let registration = selected_train.evn.marking(
        &state.region.railway_registration.mark,
        &state.player_company.vehicle_keeper_mark,
    );
    assert!(train_step.contains(&registration));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Manual Dispatch should mute the complete application underneath it",
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let service_step = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(service_step.contains("Choose Passenger Service"));
    assert!(service_step.contains("R1"));
    assert!(service_step.contains("Service Preview"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("Review Dispatch"));
    assert!(review.contains("SERVICE"));
    assert!(review.contains("R1"));

    let train_id = state.player_company.fleet.trains[0].id;
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::ManualDispatch {
            train_id,
            service_id: outbound,
        }
    );
}

#[test]
fn service_selection_only_lists_services_starting_at_the_train_location() {
    let (state, outbound, second_origin) = state_with_ready_trains_and_services();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    let first_train = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(first_train.contains("R1"));
    assert!(!first_train.contains("R2"));

    press(&mut shell, &state, KeyCode::Left);
    press(&mut shell, &state, KeyCode::Down);
    press(&mut shell, &state, KeyCode::Enter);
    let second_train = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(second_train.contains("R2"));
    assert!(!second_train.contains("R1"));

    assert_ne!(outbound, second_origin);
}

#[test]
fn dispatch_does_not_create_a_service_implicitly() {
    let mut state = create_new_game(42, "No Services", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("Train is purchased");
    let before = state.clone();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('d'));
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("No Passenger Service starts"));
    assert!(rendered.contains("create one first"));
    assert_eq!(state, before);
    assert!(state.player_company.passenger_services.is_empty());
}

#[test]
fn fleet_dispatch_opens_the_same_train_first_modal_with_the_fleet_train_selected() {
    let (state, _, _) = state_with_ready_trains_and_services();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('2'));
    let fleet = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(fleet.contains("Train 01"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("Manual Dispatch"));
    assert!(rendered.contains("1 TRAIN"));
    assert!(rendered.contains("2 SERVICE"));
    assert!(rendered.contains("3 REVIEW"));
    assert!(rendered.contains("Choose Train"));
    assert!(rendered.contains("› Train 01"));
}

#[test]
fn changed_train_state_keeps_dispatch_recoverable() {
    let (mut state, _, _) = state_with_ready_trains_and_services();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
        journey_id: railq::model::JourneyId::new(99),
    };

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("no longer READY"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Backspace),
        ShellAction::Continue
    );
    let recovered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(recovered.contains("Train 02"));
}

#[test]
fn cancellation_is_presentation_only() {
    let (state, _, _) = state_with_ready_trains_and_services();
    let before = state.clone();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    press(&mut shell, &state, KeyCode::Enter);
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(state, before);
}

#[test]
fn compact_service_chooser_remains_readable() {
    let (state, _, _) = state_with_ready_trains_and_services();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    let compact = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(compact.contains("Choose Passenger Service"));
    assert!(compact.contains("Destination"));
    assert!(compact.contains("Est. result"));
}

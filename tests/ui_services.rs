use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{RailStationId, UtcSeconds},
    sim::{services::create_service, world::create_new_game},
    ui::{Shell, ShellAction, capture_rendered_buffer_mut},
};

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn map_opens_service_workspace_and_builds_an_ordered_stop_pattern() {
    let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
    let mut shell = Shell::new();

    assert_eq!(press(&mut shell, &state, KeyCode::Char('s')), ShellAction::Continue);
    let workspace = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(workspace.contains("Passenger Services"));
    assert!(workspace.contains("No Passenger Services yet"));

    assert_eq!(press(&mut shell, &state, KeyCode::Char('n')), ShellAction::Continue);
    assert_eq!(press(&mut shell, &state, KeyCode::Enter), ShellAction::Continue);
    assert_eq!(press(&mut shell, &state, KeyCode::Down), ShellAction::Continue);
    assert_eq!(press(&mut shell, &state, KeyCode::Enter), ShellAction::Continue);
    assert_eq!(press(&mut shell, &state, KeyCode::Char('f')), ShellAction::Continue);

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::CreatePassengerService {
            stop_station_ids: vec![RailStationId::new(1), RailStationId::new(2)],
        }
    );
}

#[test]
fn existing_services_are_listed_and_can_request_deletion() {
    let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('s'));
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("R1"));
    assert!(rendered.contains("Ordered stops"));

    assert_eq!(press(&mut shell, &state, KeyCode::Char('d')), ShellAction::Continue);
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::DeletePassengerService { service_id }
    );
}

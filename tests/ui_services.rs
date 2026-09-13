use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::create_service,
        world::create_new_game,
    },
    ui::{
        Shell, ShellAction, capture_rendered_buffer_mut, capture_rendered_cell_colors,
        services::ServiceWorkspace, theme,
    },
};

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn map_opens_service_workspace_and_builds_an_ordered_stop_pattern() {
    let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('s')),
        ShellAction::Continue
    );
    let workspace = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(workspace.contains("Passenger Services"));
    assert!(workspace.contains("No Passenger Services yet"));
    assert!(!workspace.contains("Press N"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('n')),
        ShellAction::Continue
    );
    let create = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(create.contains("Create Passenger Service"));
    assert!(create.contains("1 STOPS"));
    assert!(create.contains("Route Preview"));
    assert!(create.contains("Choose the next stop"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Create Service should mute the complete application underneath it",
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
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
        press(&mut shell, &state, KeyCode::Char('f')),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("2 REVIEW"));
    assert!(review.contains("Review Passenger Service"));
    assert!(review.contains("ORDERED STOPS"));

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
    assert!(rendered.contains("Service"));
    assert!(rendered.contains("Direction"));
    assert!(rendered.contains("›"));
    assert!(rendered.contains("STATUS"));
    assert!(rendered.contains("IDLE"));
    assert!(rendered.contains("ROUTE"));
    assert!(rendered.contains("OPERATIONS"));
    assert!(rendered.contains("PASSENGERS"));
    assert!(rendered.contains("STOP PATTERN"));
    assert!(!rendered.contains("Service Details"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let confirmation = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(confirmation.contains("Delete Passenger Service"));
    assert!(confirmation.contains("Delete this Service?"));
    assert!(confirmation.contains("[Enter] delete"));
    assert!(confirmation.contains("[Esc] cancel"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::DeletePassengerService { service_id }
    );
}

#[test]
fn active_service_inspector_surfaces_live_operating_context() {
    let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
    let mut state = create_new_game(42, "Alden Passenger", started_at);
    state.player_company.funds = Money::from_cents(10_000_000);
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();

    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('s'));
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);

    assert!(rendered.contains("IN SERVICE"));
    assert!(rendered.contains("Active trains"));
    assert!(rendered.contains("Next arrival"));
    assert!(rendered.contains("Train 01"));
    assert!(rendered.contains("RUNNING TRAINS"));
    assert!(rendered.contains("Oakridge → Fairford"));
    assert!(rendered.contains("Load"));
    assert!(rendered.contains("On board"));
    assert!(rendered.contains("Carried"));
    assert!(rendered.contains("COMMERCIAL"));
    assert!(rendered.contains("Expected revenue"));
    assert!(rendered.contains("Operating cost"));
    assert!(rendered.contains("Expected result"));
    assert!(!rendered.contains("STOP PATTERN"));
}

#[test]
fn service_footer_keeps_delete_visible_but_disabled_while_service_is_active() {
    let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
    let mut state = create_new_game(42, "Alden Passenger", started_at);
    state.player_company.funds = Money::from_cents(10_000_000);
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let workspace = ServiceWorkspace::default();

    let idle_footer = workspace.footer_shortcuts(false, true, &state);
    assert_eq!(
        idle_footer
            .iter()
            .find(|(key, _, _)| *key == "E")
            .map(|(_, _, enabled)| *enabled),
        Some(true),
    );
    assert_eq!(
        idle_footer
            .iter()
            .find(|(key, _, _)| *key == "D")
            .map(|(_, _, enabled)| *enabled),
        Some(true),
    );
    assert!(
        idle_footer
            .iter()
            .any(|(key, action, enabled)| { *key == "PgUp/PgDn" && *action == "Page" && *enabled })
    );

    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();

    let active_footer = workspace.footer_shortcuts(false, true, &state);
    assert_eq!(
        active_footer
            .iter()
            .find(|(key, _, _)| *key == "E")
            .map(|(_, _, enabled)| *enabled),
        Some(false),
    );
    assert_eq!(
        active_footer
            .iter()
            .find(|(key, _, _)| *key == "D")
            .map(|(_, _, enabled)| *enabled),
        Some(false),
    );
}

#[test]
fn delete_shortcut_is_a_no_op_while_selected_service_is_active() {
    let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
    let mut state = create_new_game(42, "Alden Passenger", started_at);
    state.player_company.funds = Money::from_cents(10_000_000);
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('s'));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(!rendered.contains("Delete Passenger Service"));
    assert!(rendered.contains("IN SERVICE"));
}

#[test]
fn idle_service_uses_the_shared_editor_for_route_changes() {
    let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('s'));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('e')),
        ShellAction::Continue
    );
    let editor = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(editor.contains("Edit Passenger Service · R1"));
    assert!(editor.contains("1 STOPS"));
    assert!(editor.contains("Oakridge → Fairford"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Edit Service should use the same dimmed focused-modal treatment as Create Service",
    );

    // Remove Fairford, choose the next station in the catalogue, and save the
    // replacement stop pattern through the same STOPS → REVIEW workflow.
    press(&mut shell, &state, KeyCode::Backspace);
    press(&mut shell, &state, KeyCode::Down);
    press(&mut shell, &state, KeyCode::Enter);
    press(&mut shell, &state, KeyCode::Char('f'));
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("Review Service Changes"));
    assert!(review.contains("[Enter] save"));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::UpdatePassengerService {
            service_id,
            stop_station_ids: vec![RailStationId::new(1), RailStationId::new(3)],
        }
    );
}

#[test]
fn edit_shortcut_is_disabled_while_selected_service_is_active() {
    let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
    let mut state = create_new_game(42, "Alden Passenger", started_at);
    state.player_company.funds = Money::from_cents(10_000_000);
    let service_id = create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(2)],
    )
    .unwrap();
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();
    let mut shell = Shell::new();

    press(&mut shell, &state, KeyCode::Char('s'));
    let footer = ServiceWorkspace::default().footer_shortcuts(false, true, &state);
    assert_eq!(
        footer
            .iter()
            .find(|(key, _, _)| *key == "E")
            .map(|(_, _, enabled)| *enabled),
        Some(false),
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('e')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(!rendered.contains("Edit Passenger Service"));
    assert!(rendered.contains("IN SERVICE"));
}

//! Context-sensitive footer evidence for task 21.

use std::{fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{GameState, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    ui::{Shell, capture_rendered_buffer, capture_rendered_cell_colors},
};

const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/21";
const NOW: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

fn press(shell: &mut Shell, state: &GameState, code: KeyCode) {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state);
}

fn ready_state() -> GameState {
    let mut state = create_new_game(42, "Context Passenger", NOW);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("fixed purchase");
    find_or_create_service(
        &mut state,
        RailStationId::new(1),
        RailStationId::new(2),
    )
    .expect("fixed service");
    state
}

#[test]
fn captures_contextual_controls_and_focus_marker() {
    let evidence = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence).expect("evidence directory");

    let state = ready_state();
    let mut map = Shell::new();
    press(&mut map, &state, KeyCode::Char('d'));
    let train_step = capture_rendered_buffer(&map, &state, 120, 40);
    assert!(train_step.contains("[Enter] Next"));
    assert!(train_step.contains("[Esc] Cancel"));
    assert!(train_step.contains("FOCUS"));
    fs::write(evidence.join("dispatch-train-step-120x40.txt"), &train_step).expect("capture");

    press(&mut map, &state, KeyCode::Enter);
    let destination_step = capture_rendered_buffer(&map, &state, 80, 24);
    assert!(destination_step.contains("[←] Back"));
    assert!(destination_step.contains("[Q] Quit"));
    fs::write(
        evidence.join("dispatch-service-step-80x24.txt"),
        &destination_step,
    )
    .expect("capture");

    let mut travelling = ready_state();
    let train_id = travelling.player_company.fleet.trains[0].id;
    let service_id = find_or_create_service(
        &mut travelling,
        RailStationId::new(1),
        RailStationId::new(2),
    )
    .expect("fixed service");
    dispatch_journey(&mut travelling, train_id, service_id, NOW).expect("fixed dispatch");
    let mut fleet = Shell::new();
    press(&mut fleet, &travelling, KeyCode::Char('t'));
    let fleet_view = capture_rendered_buffer(&fleet, &travelling, 80, 24);
    assert!(fleet_view.contains("[D] no: travel"));
    assert!(fleet_view.contains("[S] no: travel"));
    fs::write(evidence.join("travelling-fleet-80x24.txt"), &fleet_view).expect("capture");

    let (fg, bg) =
        capture_rendered_cell_colors(&fleet, &travelling, 80, 24, 0, 23).expect("footer cell");
    assert_eq!(fg, railq::ui::theme::ACCENT);
    assert_eq!(bg, railq::ui::theme::PANEL);
}

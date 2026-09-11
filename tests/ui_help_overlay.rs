//! Help overlay evidence for task 22.

use std::{fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{RailStationId, UtcSeconds},
    sim::world::create_new_game,
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/22";
const NOW: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn help_is_scrollable_and_uses_a_focused_page_when_compact() {
    let evidence = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence).expect("evidence directory");

    let state = create_new_game(42, "Help Passenger", NOW);
    let mut shell = Shell::new();
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('?')),
        ShellAction::Continue
    );

    let wide = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(wide.contains("Keyboard help"));
    assert!(wide.contains("Global controls"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 12, 4),
        Some((theme::ACCENT, theme::PANEL))
    );
    fs::write(evidence.join("help-overlay-120x40.txt"), &wide).expect("wide capture");

    let compact_before = capture_rendered_buffer_mut(&mut shell, &state, 64, 16);
    assert!(compact_before.contains("Help · focused page"));
    assert!(compact_before.contains("Global controls"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let compact_after = capture_rendered_buffer_mut(&mut shell, &state, 64, 16);
    assert!(compact_after.contains("Panels and lists"));
    assert_ne!(compact_before, compact_after);
    fs::write(evidence.join("help-focused-page-64x16.txt"), &compact_after)
        .expect("compact capture");
}

#[test]
fn help_isolates_input_and_restores_the_pending_purchase_proposal() {
    let mut state = create_new_game(42, "Proposal Passenger", NOW);
    state.player_company.funds = railq::model::Money::from_cents(1_000_000);
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
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
        press(&mut shell, &state, KeyCode::Char('h')),
        ShellAction::Continue
    );
    assert!(shell.help_visible());
    let _ = capture_rendered_buffer(&shell, &state, 120, 40);
    let _ = capture_rendered_buffer(&shell, &state, 64, 16);
    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert!(!shell.help_visible());

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('?')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::PurchaseTrain {
            catalogue_index: 0,
            delivery_station_id: RailStationId::new(2),
        }
    );
}

#[test]
fn help_keeps_global_exit_available() {
    let state = create_new_game(42, "Exit Passenger", NOW);
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('?'));

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('q')),
        ShellAction::Exit
    );

    let mut control_c_shell = Shell::new();
    press(&mut control_c_shell, &state, KeyCode::Char('?'));
    assert_eq!(
        control_c_shell.handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &state,
        ),
        ShellAction::Exit
    );
}

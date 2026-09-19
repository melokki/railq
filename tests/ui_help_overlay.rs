//! Help overlay behavior tests.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{RailStationId, UtcSeconds},
    sim::world::create_new_game,
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const NOW: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn help_is_scrollable_and_uses_a_focused_page_when_compact() {
    let state = create_new_game(42, "Help Passenger", NOW);
    let mut shell = Shell::new();
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('?')),
        ShellAction::Continue
    );

    let wide = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(wide.contains("Help · Map"));
    assert!(wide.contains("Workspaces"));
    assert!(wide.contains("[PgUp/PgDn] page"));
    assert!(wide.contains("[Esc/?] close"));
    assert!(
        wide.lines().last().is_some_and(|line| line.trim().is_empty()),
        "The global footer should stay geometrically present but hide its actions while Help owns input",
    );
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Help should mute the application underneath it",
    );
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 16, 6),
        Some((theme::ACCENT, theme::PANEL)),
        "Help should use the shared focused-modal border",
    );
    let compact_before = capture_rendered_buffer_mut(&mut shell, &state, 64, 16);
    assert!(compact_before.contains("Help · Map"));
    assert!(compact_before.contains("[3] Open Market"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let compact_after = capture_rendered_buffer_mut(&mut shell, &state, 64, 16);
    assert!(compact_after.contains("Tip"));
    assert_ne!(compact_before, compact_after);
}

#[test]
fn help_isolates_input_and_restores_the_pending_purchase_proposal() {
    let mut state = create_new_game(42, "Proposal Passenger", NOW);
    state.player_company.funds = railq::model::Money::from_cents(1_000_000);
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('3')),
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

//! Rendered-buffer and keyboard coverage for the Train catalogue comparison.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::UtcSeconds,
    sim::world::create_new_game,
    ui::{Shell, ShellAction, capture_rendered_buffer_mut, capture_rendered_cell_colors, theme},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/17";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn catalogue_compares_models_and_restores_the_focused_model_after_cancellation()
-> Result<(), Box<dyn Error>> {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let before = state.clone();
    let mut shell = Shell::new();
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    for expected in [
        "Local 70",
        "Express 120",
        "$3,000.00",
        "$5,000.00",
        "70 pax",
        "120 pax",
        "90.0 km/h",
        "118.8 km/h",
        "$0.45/km",
        "$0.30/km",
        "Selected model",
    ] {
        assert!(
            wide.contains(expected),
            "wide catalogue should show {expected}"
        );
    }
    fs::write(evidence_dir.join("catalogue-120x40.txt"), &wide)?;

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for expected in [
        "Local 70",
        "Express 120",
        "$3,000.00",
        "$5,000.00",
        "70 pax",
        "120 pax",
        "90.0 km/h",
        "118.8 km/h",
        "$0.45/km",
        "$0.30/km",
    ] {
        assert!(
            compact.contains(expected),
            "80x24 catalogue should retain {expected}"
        );
    }
    fs::write(evidence_dir.join("catalogue-80x24.txt"), compact)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    let selected = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    let (row, column) = selected
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Express 120").map(|column| (row, column)))
        .expect("the selected catalogue model is visibly marked");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column)?,
            u16::try_from(row)?,
        ),
        Some((theme::BACKGROUND, theme::ACCENT))
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let delivery = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(delivery.contains("Select a delivery Rail Station"));
    assert!(delivery.contains("Selected Train: Express 120"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    let restored = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored.contains("> Express 120"));
    assert_eq!(
        state, before,
        "catalogue selection and cancellation are presentation-only"
    );
    Ok(())
}

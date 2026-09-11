//! Keyboard and render coverage for the Player Company's financial recovery path.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{GameState, Money, RailStationId, UtcSeconds},
    sim::{fleet::purchase_train, world::create_new_game},
    ui::{Shell, ShellAction, View, capture_rendered_buffer, capture_rendered_buffer_mut},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const ORIGIN: RailStationId = RailStationId::new(1);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/08";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn insolvency_fixture() -> GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    purchase_train(&mut state, 0, ORIGIN).expect("first fixture Train is purchased");
    purchase_train(&mut state, 0, ORIGIN).expect("second fixture Train is purchased");
    state.player_company.funds = Money::ZERO;
    state
}

#[test]
fn recovery_review_lists_ordered_steps_and_only_navigates_to_a_review() -> Result<(), Box<dyn Error>>
{
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let state = insolvency_fixture();
    let before = state.clone();
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('c')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('r')), &state),
        ShellAction::Continue
    );

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Finite recovery routes"));
    assert!(wide.contains("Ordered recovery instructions"));
    assert!(wide.contains("Retain Train"));
    assert!(wide.contains("Review resale"));
    assert!(wide.contains("Review Manual Dispatch"));
    assert!(wide.contains("Nothing is sold, bought, or dispatched here."));
    fs::write(evidence_dir.join("recovery-review-120x40.txt"), wide)?;

    let compact = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(compact.contains("Finite recovery routes"));
    assert!(compact.contains("Ordered recovery"));
    assert!(compact.contains("Nothing is sold, bought, or dispatched"));
    fs::write(evidence_dir.join("recovery-review-80x24.txt"), compact)?;

    for _ in 0..6 {
        assert_eq!(
            shell.handle_key(key(KeyCode::Down), &state),
            ShellAction::Continue
        );
    }
    let rebuy = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rebuy.contains("Sell Fleet; rebuy"));
    assert!(rebuy.contains("Review purchase"));
    assert!(rebuy.contains("Buy Trains"));
    fs::write(evidence_dir.join("recovery-rebuy-120x40.txt"), rebuy)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    assert_eq!(shell.active_view(), View::Trains);
    assert_eq!(
        state, before,
        "opening recovery guidance must not mutate play"
    );
    Ok(())
}

#[test]
fn bankruptcy_restart_review_keeps_help_cancel_failure_and_saved_outcome_explicit()
-> Result<(), Box<dyn Error>> {
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::ZERO;
    let before = state.clone();
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('?')), &state),
        ShellAction::Continue
    );
    assert!(shell.help_visible());
    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    assert!(!shell.help_visible());

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('r')), &state),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer(&shell, &state, 120, 40);
    fs::write(
        evidence_dir.join("bankruptcy-restart-review-120x40.txt"),
        &review,
    )?;
    assert!(review.contains("BANKRUPTCY"));
    assert!(review.contains("Safe restart review"));
    assert!(review.contains("preserved"));

    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    assert_eq!(
        state, before,
        "cancelling restart must preserve the company"
    );

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('r')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::RestartAfterBankruptcy
    );
    shell.reject_restart_after_bankruptcy("simulated save rejection");
    let failed = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(failed.contains("existing save was not overwritten"));
    assert_eq!(
        state, before,
        "a rejected restart must preserve the company"
    );
    fs::write(
        evidence_dir.join("bankruptcy-save-rejected-120x40.txt"),
        failed,
    )?;

    shell.confirm_restart_after_bankruptcy();
    let replacement = create_new_game(7, "Northstar Passenger", STARTED_AT);
    let saved = capture_rendered_buffer(&shell, &replacement, 120, 40);
    assert!(saved.contains("Fresh game saved"));
    assert!(saved.contains("restart backup"));
    fs::write(evidence_dir.join("bankruptcy-restarted-120x40.txt"), saved)?;
    Ok(())
}

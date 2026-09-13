//! Interaction and rendered-buffer coverage for retained Journey receipts.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{GameState, JourneyId, JourneyReceipt, Money, UtcSeconds},
    sim::world::create_new_game,
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/07";

fn retained_receipts_fixture() -> GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.financials.recent_journey_receipts = (1..=18)
        .map(|id| JourneyReceipt {
            journey_id: JourneyId::new(id),
            revenue: Money::from_cents((id as i64) * 10_000),
            infrastructure_access_fee: Money::from_cents((id as i64) * 1_000),
            fuel_cost: Money::from_cents((id as i64) * 250),
        })
        .collect();
    state
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn company_shell(state: &GameState) -> Shell {
    let mut shell = Shell::new();
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('c')), state),
        ShellAction::Continue
    );
    shell
}

#[test]
fn retained_receipts_scroll_keep_the_selected_journey_across_arrivals_and_open_detail()
-> Result<(), Box<dyn Error>> {
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let mut state = retained_receipts_fixture();
    let mut shell = company_shell(&state);

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("JOURNEY HISTORY · 18 RECEIPTS"));
    assert!(wide.contains("J18"));
    assert!(wide.contains("Revenue"));
    assert!(wide.contains("Access fees"));
    assert!(wide.contains("Fuel"));
    assert!(wide.contains("Result"));
    let (selected_x, selected_y) = text_position(&wide, "J18")
        .expect("selected receipt should be visible in Journey history");
    let selected_colors = capture_rendered_cell_colors(
        &shell,
        &state,
        120,
        40,
        selected_x,
        selected_y,
    )
    .expect("selected receipt should paint its Journey cell");
    assert_eq!(selected_colors, (theme::BACKGROUND, theme::ACCENT));
    fs::write(evidence_dir.join("retained-120x40.txt"), wide)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::PageDown), &state),
        ShellAction::Continue
    );
    let scrolled = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(
        scrolled.contains("J06"),
        "PageDown must reach receipts past row five"
    );

    state
        .financials
        .recent_journey_receipts
        .push(JourneyReceipt {
            journey_id: JourneyId::new(19),
            revenue: Money::from_cents(190_000),
            infrastructure_access_fee: Money::from_cents(19_000),
            fuel_cost: Money::from_cents(4_750),
        });
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    let detail = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(detail.contains("Journey 6 · retained receipt"));
    assert!(detail.contains("$600.00"));
    assert!(detail.contains("$60.00"));
    assert!(detail.contains("$15.00"));
    assert!(detail.contains("+$525.00"));
    fs::write(evidence_dir.join("detail-120x40.txt"), detail)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    let restored = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(
        restored.contains("> J06"),
        "Esc must restore the selected receipt"
    );

    let compact = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(compact.contains("JOURNEY HISTORY · 19 RECEIPTS"));
    assert!(compact.contains("[↑↓] Receipt [Enter] Inspect"));
    fs::write(evidence_dir.join("retained-80x24.txt"), compact)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    let compact_detail = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_detail.contains("Journey 6 · retained receipt"));
    assert!(compact_detail.contains("+$525.00"));
    fs::write(evidence_dir.join("detail-80x24.txt"), compact_detail)?;
    Ok(())
}

#[test]
fn empty_retained_history_explains_when_a_receipt_is_created() {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let shell = company_shell(&state);
    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);

    assert!(rendered.contains("JOURNEY HISTORY · 0 RECEIPTS"));
    assert!(rendered.contains("No retained Journey receipts yet."));
    assert!(rendered.contains("Operating Revenue"));
    assert!(rendered.contains("arrives."));
    assert!(!rendered.to_lowercase().contains("chart"));
}

fn text_position(rendered: &str, needle: &str) -> Option<(u16, u16)> {
    rendered.lines().enumerate().find_map(|(row, line)| {
        line.find(needle).map(|byte_index| {
            let column = line[..byte_index].chars().count();
            (column as u16, row as u16)
        })
    })
}

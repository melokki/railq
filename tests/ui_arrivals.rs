//! Saved live-arrival feedback at the terminal shell boundary.

use std::{
    cell::{Cell, RefCell},
    error::Error,
    fmt, fs,
    path::Path,
    rc::Rc,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    app::{App, GameStore},
    model::{GameState, Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        time::advance_time, world::create_new_game,
    },
    ui::{Shell, capture_rendered_buffer},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/24";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn travelling_state() -> GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    let first_train = purchase_train(&mut state, 0, RailStationId::new(1)).expect("first train");
    let second_train = purchase_train(&mut state, 0, RailStationId::new(1)).expect("second train");
    let service = find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
        .expect("service");
    dispatch_journey(&mut state, first_train, service, STARTED_AT).expect("first Journey");
    dispatch_journey(&mut state, second_train, service, STARTED_AT).expect("second Journey");
    state
}

#[test]
fn committed_multiple_arrivals_are_inspectable_once_with_current_funds()
-> Result<(), Box<dyn Error>> {
    let before = travelling_state();
    let arrives_at = before.active_journeys[0].arrives_at;
    let mut after = before.clone();
    advance_time(&mut after, arrives_at).expect("arrival reconciliation");
    let mut shell = Shell::new();

    shell.publish_committed_arrivals(&before, &after);
    let summary = capture_rendered_buffer(&shell, &after, 120, 40);
    assert!(summary.contains("2 Journeys arrived"));
    assert!(summary.contains("Company Funds"));
    assert!(summary.contains("[Enter] Read"));
    fs::create_dir_all(EVIDENCE_DIR)?;
    fs::write(
        Path::new(EVIDENCE_DIR).join("arrival-summary-120x40.txt"),
        &summary,
    )?;

    shell.handle_key(key(KeyCode::Enter), &after);
    let details = capture_rendered_buffer(&shell, &after, 120, 40);
    assert!(details.contains("Arrival summary"));
    assert!(details.contains("Train 01 arrived at"));
    assert!(details.contains("Train 02 arrived at"));
    assert!(details.contains("Rail Station"));
    assert!(details.contains("Current Company Funds:"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("arrival-details-120x40.txt"),
        &details,
    )?;
    let compact = capture_rendered_buffer(&shell, &after, 80, 24);
    assert!(compact.contains("Arrival summary"));
    assert!(compact.contains("Train 01 arrived at"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("arrival-details-80x24.txt"),
        &compact,
    )?;

    shell.handle_key(key(KeyCode::Char('a')), &after);
    shell.publish_committed_arrivals(&after, &after);
    let repeated = capture_rendered_buffer(&shell, &after, 120, 40);
    assert!(!repeated.contains("arrived"));
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct RejectingStore {
    saved: Rc<RefCell<Option<GameState>>>,
    reject_next_save: Rc<Cell<bool>>,
}

#[derive(Clone, Copy, Debug)]
struct RejectedSave;

impl fmt::Display for RejectedSave {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("simulated save rejection")
    }
}

impl Error for RejectedSave {}

impl GameStore for RejectingStore {
    type Error = RejectedSave;

    fn load(&self) -> Result<Option<GameState>, Self::Error> {
        Ok(self.saved.borrow().clone())
    }

    fn save(&self, state: &GameState) -> Result<(), Self::Error> {
        if self.reject_next_save.replace(false) {
            return Err(RejectedSave);
        }
        self.saved.replace(Some(state.clone()));
        Ok(())
    }
}

#[test]
fn rejected_reconciliation_publishes_no_arrival_or_revenue_notice() {
    let store = RejectingStore::default();
    let state = travelling_state();
    let arrives_at = state.active_journeys[0].arrives_at;
    let mut app = App::start_new(store.clone(), state).expect("initial save");
    let before = app.state().clone();
    let mut shell = Shell::new();

    store.reject_next_save.set(true);
    assert!(app.reconcile(arrives_at).is_err());
    assert_eq!(app.state(), &before);
    shell.publish_committed_arrivals(&before, app.state());

    let rendered = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(!rendered.contains("arrived"));
    assert!(!rendered.contains("credited"));
}

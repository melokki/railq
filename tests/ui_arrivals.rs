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
    ui::{
        Shell, capture_rendered_buffer,
        start::{Startup, start},
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/24";
const STARTUP_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/24a";

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
fn committed_multiple_arrivals_are_non_blocking_with_current_funds()
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
    assert!(!summary.contains("[Enter] Read"));
    fs::create_dir_all(EVIDENCE_DIR)?;
    fs::write(
        Path::new(EVIDENCE_DIR).join("arrival-summary-120x40.txt"),
        &summary,
    )?;

    // Routine arrival feedback never captures the keyboard. A normal Map
    // command works immediately and replaces the passive message.
    shell.handle_key(key(KeyCode::Char('w')), &after);
    let continued = capture_rendered_buffer(&shell, &after, 120, 40);
    assert!(continued.contains("World Details"));
    assert!(!continued.contains("2 Journeys arrived"));

    shell.handle_key(key(KeyCode::Esc), &after);
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

#[test]
fn startup_arrivals_use_a_non_blocking_live_summary_once_after_the_save_succeeds() -> Result<(), Box<dyn Error>>
{
    let store = RejectingStore::default();
    let state = travelling_state();
    let arrives_at = state.active_journeys[0].arrives_at;
    App::start_new(store.clone(), state.clone()).expect("initial save");

    let Startup::Dashboard(dashboard) = start(store.clone(), arrives_at).expect("startup") else {
        panic!("saved Player Company opens the dashboard");
    };
    let (app, arrivals) = dashboard.into_parts();
    assert_eq!(arrivals.len(), 2);
    assert!(app.state().active_journeys.is_empty());
    let mut shell = Shell::new();
    shell.publish_settled_arrivals(app.state(), &arrivals);
    let summary = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(summary.contains("2 Journeys arrived"));
    assert!(summary.contains("Company Funds"));
    assert!(!summary.contains("[Enter] Read"));
    fs::create_dir_all(STARTUP_EVIDENCE_DIR)?;
    fs::write(
        Path::new(STARTUP_EVIDENCE_DIR).join("offline-arrival-summary-120x40.txt"),
        &summary,
    )?;

    shell.handle_key(key(KeyCode::Char('w')), app.state());
    let continued = capture_rendered_buffer(&shell, app.state(), 80, 24);
    assert!(continued.contains("World Details"));
    assert!(!continued.contains("2 Journeys arrived"));
    drop(app);

    let Startup::Dashboard(dashboard) = start(store, arrives_at).expect("reopen") else {
        panic!("saved Player Company opens the dashboard");
    };
    let (app, arrivals) = dashboard.into_parts();
    assert!(arrivals.is_empty());
    let shell = Shell::new();
    let reopened = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(!reopened.contains("arrived"));
    Ok(())
}

#[test]
fn failed_startup_save_has_no_arrival_summary() {
    let store = RejectingStore::default();
    let state = travelling_state();
    let arrives_at = state.active_journeys[0].arrives_at;
    App::start_new(store.clone(), state.clone()).expect("initial save");
    store.reject_next_save.set(true);

    assert!(start(store.clone(), arrives_at).is_err());
    assert_eq!(store.load().expect("saved state"), Some(state));
}

#[test]
fn startup_before_eta_has_no_completed_arrival() {
    let store = RejectingStore::default();
    let state = travelling_state();
    let before_eta =
        UtcSeconds::from_unix_seconds(state.active_journeys[0].arrives_at.unix_seconds() - 1);
    App::start_new(store.clone(), state).expect("initial save");

    let Startup::Dashboard(dashboard) = start(store, before_eta).expect("startup") else {
        panic!("saved Player Company opens the dashboard");
    };
    let (app, arrivals) = dashboard.into_parts();
    assert!(arrivals.is_empty());
    assert_eq!(app.state().active_journeys.len(), 2);
}

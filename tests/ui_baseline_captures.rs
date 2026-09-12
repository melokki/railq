//! Deterministic rendered-buffer captures for the pre-redesign playable UI.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    app::App,
    model::{GameState, Money, RailStationId, UtcSeconds},
    sim::{
        economy::quote_journey, fleet::purchase_train, journeys::dispatch_journey,
        services::find_or_create_service, time::advance_time, world::create_new_game,
    },
    storage::SaveSlot,
    ui::{Shell, ShellAction, capture_rendered_buffer},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const DEPARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_600);
const ORIGIN: RailStationId = RailStationId::new(1);
const DESTINATION: RailStationId = RailStationId::new(2);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/01";
const SIZES: [(u16, u16, &str); 2] = [(120, 40, "120x40"), (80, 24, "80x24")];

#[derive(Clone)]
struct Scenario {
    slug: &'static str,
    note: &'static str,
    shell: Shell,
    state: GameState,
}

#[test]
fn captures_the_playable_ui_baseline() -> Result<(), Box<dyn Error>> {
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;

    let empty = persisted_state("empty", new_state())?;
    let ready = persisted_state("ready", ready_state())?;
    let settled = persisted_state("settled", settled_state())?;

    let mut scenarios = vec![
        Scenario {
            slug: "empty_map",
            note: "Map before the Fleet exists; the empty state points to Buy Trains.",
            shell: shell_for_view(&empty, 'm'),
            state: empty.clone(),
        },
        Scenario {
            slug: "empty_fleet",
            note: "Fleet before a Train purchase; no scrolling or secondary inspector exists.",
            shell: shell_for_view(&empty, 't'),
            state: empty.clone(),
        },
        Scenario {
            slug: "empty_company",
            note: "Company view with starting funds and no Journey receipts.",
            shell: shell_for_view(&empty, 'c'),
            state: empty.clone(),
        },
        Scenario {
            slug: "empty_buy_trains",
            note: "Buy Trains catalogue before any proposal is started.",
            shell: shell_for_view(&empty, 'b'),
            state: empty.clone(),
        },
        Scenario {
            slug: "operating_map",
            note: "Map after one settled Journey; all content is still text inside the content panel.",
            shell: shell_for_view(&settled, 'm'),
            state: settled.clone(),
        },
        Scenario {
            slug: "operating_fleet",
            note: "Fleet after arrival; resale affordance is appended below the Train text.",
            shell: shell_for_view(&settled, 't'),
            state: settled.clone(),
        },
        Scenario {
            slug: "operating_company",
            note: "Company view after one receipt; long financial lines clip in compact width.",
            shell: shell_for_view(&settled, 'c'),
            state: settled.clone(),
        },
        Scenario {
            slug: "operating_buy_trains",
            note: "Buy Trains view while a Train already exists; catalogue remains globally available.",
            shell: shell_for_view(&settled, 'b'),
            state: settled.clone(),
        },
        Scenario {
            slug: "purchase_review",
            note: "Final purchase review for the first diesel Train delivered to the first Rail Station.",
            shell: purchase_review_shell(&empty),
            state: empty.clone(),
        },
        Scenario {
            slug: "dispatch_review",
            note: "Final Manual Dispatch review; Passenger Service creation is only previewed.",
            shell: dispatch_review_shell(&ready),
            state: ready.clone(),
        },
        Scenario {
            slug: "resale_review",
            note: "Final resale review for a READY Train; sale proceeds are visible before confirmation.",
            shell: resale_review_shell(&ready),
            state: ready.clone(),
        },
        Scenario {
            slug: "dispatch_error_no_ready",
            note: "Map-level error when Manual Dispatch starts without any READY Train.",
            shell: dispatch_error_shell(&empty),
            state: empty.clone(),
        },
        Scenario {
            slug: "resale_error_no_ready",
            note: "Fleet-level error when resale starts without any READY Train.",
            shell: resale_error_shell(&empty),
            state: empty.clone(),
        },
    ];
    scenarios.push(Scenario {
        slug: "dispatch_rejected_review",
        note: "Application-boundary dispatch rejection kept inside the open review.",
        shell: rejected_dispatch_review_shell(&ready)?,
        state: ready,
    });

    for scenario in &scenarios {
        for (columns, rows, label) in SIZES {
            let rendered = capture_rendered_buffer(&scenario.shell, &scenario.state, columns, rows);
            assert_eq!(rendered.lines().count(), usize::from(rows));
            assert!(
                rendered.contains("RailQ"),
                "{} at {label} should include the shell title",
                scenario.slug
            );
            let path = evidence_dir.join(format!("{}-{}.txt", scenario.slug, label));
            fs::write(path, rendered)?;
        }
    }

    fs::write(
        evidence_dir.join("before-redesign-notes.md"),
        notes(&scenarios),
    )?;
    Ok(())
}

fn new_state() -> GameState {
    create_new_game(42, "Baseline Passenger", STARTED_AT)
}

fn ready_state() -> GameState {
    let mut state = new_state();
    purchase_train(&mut state, 0, ORIGIN).expect("fixed baseline purchase should succeed");
    state
}

fn settled_state() -> GameState {
    let mut state = ready_state();
    let train_id = state.player_company.fleet.trains[0].id;
    let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION)
        .expect("fixed baseline service should be valid");
    dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT)
        .expect("fixed baseline dispatch should succeed");
    let arrives_at = state.active_journeys[0].arrives_at;
    advance_time(&mut state, arrives_at).expect("fixed baseline arrival should settle");
    state
}

fn persisted_state(name: &str, state: GameState) -> Result<GameState, Box<dyn Error>> {
    let path = temporary_save_path(name);
    let lock_path = sidecar_lock_path(&path);
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&lock_path);

    let slot = SaveSlot::open(path.clone())?;
    let app = App::start_new(slot, state)?;
    let persisted = app.state().clone();
    drop(app);

    let _ = fs::remove_file(path);
    let _ = fs::remove_file(lock_path);
    Ok(persisted)
}

fn temporary_save_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "railq-ui-baseline-{}-{name}.db",
        std::process::id()
    ))
}

fn sidecar_lock_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .expect("temporary save path should have a file name")
        .to_string_lossy();
    path.with_file_name(format!("{file_name}.lock"))
}

fn shell_for_view(state: &GameState, key: char) -> Shell {
    let mut shell = Shell::new();
    press(&mut shell, state, KeyCode::Char(key));
    shell
}

fn purchase_review_shell(state: &GameState) -> Shell {
    let mut shell = shell_for_view(state, 'b');
    press(&mut shell, state, KeyCode::Enter);
    press(&mut shell, state, KeyCode::Enter);
    press(&mut shell, state, KeyCode::Enter);
    shell
}

fn dispatch_review_shell(state: &GameState) -> Shell {
    let mut shell = shell_for_view(state, 'm');
    press(&mut shell, state, KeyCode::Char('d'));
    press(&mut shell, state, KeyCode::Enter);
    press(&mut shell, state, KeyCode::Enter);
    shell
}

fn resale_review_shell(state: &GameState) -> Shell {
    let mut shell = shell_for_view(state, 't');
    press(&mut shell, state, KeyCode::Enter);
    press(&mut shell, state, KeyCode::Enter);
    shell
}

fn dispatch_error_shell(state: &GameState) -> Shell {
    let mut shell = shell_for_view(state, 'm');
    press(&mut shell, state, KeyCode::Char('d'));
    shell
}

fn resale_error_shell(state: &GameState) -> Shell {
    let mut shell = shell_for_view(state, 't');
    press(&mut shell, state, KeyCode::Enter);
    shell
}

fn rejected_dispatch_review_shell(state: &GameState) -> Result<Shell, Box<dyn Error>> {
    let mut shell = dispatch_review_shell(state);
    let action = press(&mut shell, state, KeyCode::Enter);
    let ShellAction::ManualDispatch {
        train_id,
        destination_station_id,
    } = action
    else {
        return Err("dispatch review did not produce a confirmation action".into());
    };

    let mut rejected_state = state.clone();
    let mut preview = rejected_state.clone();
    let service_id = find_or_create_service(&mut preview, ORIGIN, destination_station_id)?;
    let quote = quote_journey(&preview, train_id, service_id)?;
    rejected_state.player_company.funds = Money::from_cents(quote.operating_cost.cents() - 1);

    let path = temporary_save_path("dispatch-rejection");
    let lock_path = sidecar_lock_path(&path);
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&lock_path);
    let slot = SaveSlot::open(path.clone())?;
    let mut app = App::start_new(slot, rejected_state)?;
    let error = app
        .dispatch_to_destination(train_id, destination_station_id, DEPARTED_AT)
        .expect_err("underfunded dispatch should be rejected");
    shell.reject_manual_dispatch(error.to_string());
    drop(app);
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(lock_path);
    Ok(shell)
}

fn press(shell: &mut Shell, state: &GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

fn notes(scenarios: &[Scenario]) -> String {
    let mut output = String::from(
        "# Task 01 before-redesign notes\n\n\
         Captures use world seed `42`, injected UTC times `1700000000` and `1700000600`, \
         and temporary save slots under the system temp directory. Each `.txt` file is a \
         direct Ratatui rendered-buffer capture; trailing cells are preserved so existing \
         terminal clipping remains visible.\n\n\
         ## Observations\n\n\
         - The current UI is a Ratatui shell, but most content is preformatted text inside a single content panel.\n\
         - The navigation, content, and footer adapt at the 64x16 minimum; below that the resize fallback is the only rendered view.\n\
         - Compact captures show honest clipping of long financial and operating lines; there is no scrolling or pagination yet.\n\
         - Empty and error states are footer or text-message driven rather than structured panels.\n\
         - Final purchase, Manual Dispatch, and resale reviews are visible and remain presentation-only until confirmation.\n\n\
         ## Capture index\n\n",
    );
    for scenario in scenarios {
        output.push_str(&format!("- `{}`: {}\n", scenario.slug, scenario.note));
    }
    output
}

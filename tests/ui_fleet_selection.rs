//! Keyboard and rendered-buffer coverage for Fleet browsing and inspection.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, TrainModelId, TrainStatus, UtcSeconds},
    sim::{
        fleet::{purchase_train, sell_train},
        journeys::dispatch_journey,
        services::{assign_train_to_service, find_or_create_service},
        time::advance_time,
        world::create_new_game,
    },
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/03";
const DETAILS_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/04";
const RESALE_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/05";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

fn operating_fleet() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    let travelling_train = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    for _ in 0..15 {
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    }
    let service =
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2)).unwrap();
    assign_train_to_service(&mut state, travelling_train, service).unwrap();
    dispatch_journey(&mut state, travelling_train, service, STARTED_AT).unwrap();
    state
}

#[test]
fn fleet_selection_is_keyboard_scrollable_and_survives_live_updates() {
    let mut state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Fleet"));
    assert!(wide.contains("FLEET AVAILABLE"));
    assert!(wide.contains("AVAILABILITY"));
    assert!(wide.contains("ALLOCATION"));
    assert!(wide.contains("LOAD"));
    assert!(wide.contains("ASSET VALUE"));
    assert!(wide.contains("ROLLING STOCK"));
    assert!(wide.contains("SELECTED TRAIN"));
    assert!(wide.contains("Train"));
    assert!(wide.contains("EVN"));
    assert!(wide.contains("Helvetra R70"));
    assert!(wide.contains("Diesel"));
    assert!(wide.contains("JOURNEY"));
    assert!(wide.contains("Current leg"));
    assert!(wide.contains("SERVICE"));
    assert!(wide.contains("CAPABILITY"));
    assert!(wide.contains("TRAVELLING"));
    assert!(wide.contains("next arrival in"));
    let travelling_row = wide
        .lines()
        .find(|line| line.contains("Train 01"))
        .expect("travelling Train is visible in the Fleet table");
    assert!(travelling_row.contains("R1"));
    assert!(!wide.contains("ACTIONS"));
    assert!(wide.contains("[R] Rename"));

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact.contains("› Train 01  TRAVELLING · R1"));

    press(&mut shell, &state, KeyCode::Down);
    let down = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(down.contains("› Train 02  READY"));
    press(&mut shell, &state, KeyCode::Char('j'));
    let j = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(j.contains("› Train 03  READY"));
    press(&mut shell, &state, KeyCode::Char('k'));
    let k = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(k.contains("› Train 02  READY"));

    press(&mut shell, &state, KeyCode::PageDown);
    let page_down = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(page_down.contains("› Train 06  READY"));
    press(&mut shell, &state, KeyCode::PageDown);
    let beyond_viewport = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(beyond_viewport.contains("› Train 10  READY"));
    assert!(!beyond_viewport.contains("› Train 01"));
    press(&mut shell, &state, KeyCode::PageUp);
    let page_up = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(page_up.contains("› Train 06  READY"));

    let arrives_at = state.active_journeys[0].arrives_at;
    advance_time(&mut state, arrives_at).unwrap();
    let after_arrival = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(after_arrival.contains("› Train 06  READY"));

    state.player_company.fleet.trains.reverse();
    let after_reorder = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(after_reorder.contains("› Train 06  READY"));
}

#[test]
fn fleet_footer_owns_actions_and_mutes_unavailable_train_actions() {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    let travelling = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(travelling.contains("[R] Rename"));
    assert!(travelling.contains("[D] Dispatch"));
    assert!(travelling.contains("[S] Sell"));
    assert!(!travelling.contains("no: travel"));
    assert!(!travelling.contains("ACTIONS"));

    let (row, column) = travelling
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("[D] Dispatch").map(|column| (row, column)))
        .expect("travelling Train keeps Dispatch visible in the footer");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column).unwrap(),
            u16::try_from(row).unwrap(),
        ),
        Some((theme::SECONDARY, theme::PANEL)),
    );

    press(&mut shell, &state, KeyCode::Down);
    let ready = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(ready.contains("[A] Assign"));
    let (row, column) = ready
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("[D] Dispatch").map(|column| (row, column)))
        .expect("unassigned READY Train keeps Dispatch visible in the footer");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column).unwrap(),
            u16::try_from(row).unwrap(),
        ),
        Some((theme::SECONDARY, theme::PANEL)),
    );
}

#[test]
fn fleet_inspector_surfaces_state_specific_information() {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    let travelling = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    let selected_train = &state.player_company.fleet.trains[0];
    assert!(travelling.contains(&selected_train.evn.formatted()));
    assert!(travelling.contains(&format!(
        "{}-{}",
        state.region.railway_registration.mark,
        state.player_company.vehicle_keeper_mark.as_str(),
    )));
    assert!(travelling.contains("SELECTED TRAIN"));
    assert!(travelling.contains("TRAVELLING"));
    assert!(travelling.contains("JOURNEY"));
    assert!(travelling.contains("SERVICE"));
    assert!(travelling.contains("PASSENGERS"));
    assert!(travelling.contains("On board"));
    assert!(travelling.contains("% load"));
    assert!(travelling.contains("ETA"));
    assert!(travelling.contains("Leg progress"));
    assert!(travelling.contains("COMMERCIAL"));
    assert!(travelling.contains("Expected result"));
    assert!(travelling.contains("CAPABILITY"));
    assert!(travelling.contains("EVN"));
    assert!(!travelling.contains("ASSET VALUE"));

    press(&mut shell, &state, KeyCode::Down);
    let ready = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(ready.contains("READY"));
    assert!(ready.contains("OPERATIONS"));
    assert!(ready.contains("Assignment required"));
    assert!(ready.contains("Next action"));
    assert!(ready.contains("Assign Passenger Service"));
    assert!(ready.contains("ASSIGNMENT"));
    assert!(ready.contains("CAPABILITY"));
    assert!(ready.contains("ASSET VALUE"));
    assert!(ready.contains("EVN"));
    assert!(!ready.contains("JOURNEY"));
}

#[test]
fn fleet_browser_states_missing_details_explicitly_and_captures_task_evidence()
-> Result<(), Box<dyn Error>> {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    for _ in 0..9 {
        press(&mut shell, &state, KeyCode::Down);
    }

    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    for (columns, rows, file_name) in [
        (120, 40, "fleet-selected-120x40.txt"),
        (80, 24, "fleet-selected-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("› Train 10"));
        fs::write(evidence_dir.join(file_name), rendered)?;
    }

    let mut missing = create_new_game(42, "Missing-data Passenger", STARTED_AT);
    missing.player_company.funds = Money::from_cents(1_000_000);
    purchase_train(&mut missing, 0, RailStationId::new(1))?;
    missing.player_company.fleet.trains[0].model_id = TrainModelId::new("missing-model");
    missing.player_company.fleet.trains[0].status = TrainStatus::Ready {
        at: RailStationId::new(99),
    };
    let mut missing_shell = Shell::new();
    press(&mut missing_shell, &missing, KeyCode::Char('2'));
    let missing_render = capture_rendered_buffer(&missing_shell, &missing, 120, 40);
    assert!(missing_render.contains("Unknown model (missing-model)"));
    assert!(missing_render.contains("Missing Rail Station 99"));

    let empty = create_new_game(42, "Empty Fleet Passenger", STARTED_AT);
    let mut empty_shell = Shell::new();
    press(&mut empty_shell, &empty, KeyCode::Char('2'));
    let empty_render = capture_rendered_buffer(&empty_shell, &empty, 80, 24);
    assert!(empty_render.contains("No rolling stock"));
    assert!(empty_render.contains("[3] Open Market"));
    Ok(())
}

#[test]
fn fleet_details_preserve_identity_and_return_to_a_predictable_list_row()
-> Result<(), Box<dyn Error>> {
    let mut state = operating_fleet();
    let journey = state.active_journeys[0].clone();
    state.last_processed_at = UtcSeconds::from_unix_seconds(
        journey.departed_at.unix_seconds()
            + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
    );
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    let _ = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);

    press(&mut shell, &state, KeyCode::Enter);
    let evidence_dir = Path::new(DETAILS_EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    for (columns, rows, file_name) in [
        (120, 40, "fleet-details-120x40.txt"),
        (80, 24, "fleet-details-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("Train details"));
        assert!(rendered.contains("PASSENGERS"));
        assert!(rendered.contains("Leg progress"));
        assert!(rendered.contains("ETA"));
        assert!(rendered.contains("Expected result"));
        fs::write(evidence_dir.join(file_name), rendered)?;
    }

    press(&mut shell, &state, KeyCode::Esc);
    advance_time(&mut state, journey.arrives_at)?;
    let list = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(list.contains("› Train 01"));
    assert!(list.contains("READY"));
    let (selected_row, selected_column) = list
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("› Train 01").map(|column| (row, column)))
        .expect("selected Train must remain visible after closing details");
    let colors = capture_rendered_cell_colors(
        &shell,
        &state,
        120,
        40,
        u16::try_from(selected_column).expect("test terminal fits u16"),
        u16::try_from(selected_row).expect("test terminal fits u16"),
    )
    .expect("selected row cell is in the rendered terminal");
    assert_eq!(colors, (theme::BACKGROUND, theme::ACCENT));

    press(&mut shell, &state, KeyCode::Down);
    let removed_train = state.player_company.fleet.trains[1].id;
    state.player_company.fleet.trains.remove(1);
    let after_removal = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(!after_removal.contains(&format!("Train {:02}", removed_train.get())));
    assert!(after_removal.contains("› Train 03"));
    Ok(())
}

#[test]
fn fleet_rename_uses_the_shared_focused_modal_treatment() {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    press(&mut shell, &state, KeyCode::Char('r'));

    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(rendered.contains("Rename Train"));
    assert!(rendered.contains("[Enter] save"));
    assert!(rendered.contains("[Esc] cancel"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Train rename should mute the complete application underneath it",
    );
}

#[test]
fn fleet_focus_respects_the_visible_workspace_and_s_reviews_the_selected_ready_train() {
    let mut state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    let _ = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    press(&mut shell, &state, KeyCode::Tab);
    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(!compact.contains("Train details"));

    let _ = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    press(&mut shell, &state, KeyCode::Tab);
    let wide_details = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide_details.contains("Train details"));
    press(&mut shell, &state, KeyCode::BackTab);

    press(&mut shell, &state, KeyCode::Down);
    press(&mut shell, &state, KeyCode::Char('s'));
    let resale = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(resale.contains("Confirm Train Resale"));
    assert!(resale.contains("Resell Train 02"));
    assert!(resale.contains("Proceeds"));
    assert!(resale.contains("Funds after"));
    assert!(resale.contains("[Enter] resell"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Train resale should mute the complete application underneath it",
    );

    let train_id = state.player_company.fleet.trains[1].id;
    assert_eq!(
        shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
        ShellAction::SellTrain { train_id }
    );
    let proceeds = sell_train(&mut state, train_id).unwrap();
    shell.confirm_train_resale(proceeds);
    let after_resale = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(after_resale.contains("› Train 03"));
    assert!(after_resale.contains("Train resold and saved"));
}

#[test]
fn resale_review_has_complete_themed_evidence_at_normal_and_compact_sizes()
-> Result<(), Box<dyn Error>> {
    let state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));
    press(&mut shell, &state, KeyCode::Down);
    press(&mut shell, &state, KeyCode::Char('s'));

    let evidence_dir = Path::new(RESALE_EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    for (columns, rows, file_name) in [
        (120, 40, "resale-review-120x40.txt"),
        (80, 24, "resale-review-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer(&shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        for fact in [
            "Resell Train 02",
            "Proceeds",
            "70%",
            "Funds now",
            "Funds after",
            "[Enter] resell",
            "[Esc] cancel",
        ] {
            assert!(
                rendered.contains(fact),
                "{fact} must fit at {columns}x{rows}"
            );
        }
        let (row, column) = rendered
            .lines()
            .enumerate()
            .find_map(|(row, line)| line.find("[Enter] resell").map(|column| (row, column)))
            .expect("confirmation instruction must be visible");
        assert_eq!(
            capture_rendered_cell_colors(
                &shell,
                &state,
                columns,
                rows,
                u16::try_from(column)?,
                u16::try_from(row)?,
            ),
            Some((theme::ACCENT, theme::PANEL)),
        );
        fs::write(evidence_dir.join(file_name), rendered)?;
    }
    Ok(())
}

#[test]
fn resale_review_explains_travelling_and_stale_selected_train_without_a_false_success() {
    let mut state = operating_fleet();
    let mut shell = Shell::new();
    press(&mut shell, &state, KeyCode::Char('2'));

    press(&mut shell, &state, KeyCode::Char('s'));
    let travelling = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(travelling.contains("TRAVELLING and cannot be resold"));

    press(&mut shell, &state, KeyCode::Down);
    press(&mut shell, &state, KeyCode::Char('s'));
    let selected_train = state.player_company.fleet.trains[1].id;
    state.player_company.fleet.trains.remove(1);
    assert_eq!(
        shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state,),
        ShellAction::Continue
    );
    let stale = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(stale.contains(&format!(
        "Train {} is no longer in the Fleet",
        selected_train.get()
    )));
}

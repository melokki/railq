//! Deterministic visual evidence for task 06's Company finance dashboard.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    balance::BalanceConfig,
    model::{
        GameState, MarketMaturity, Money, MoneyPerKilometre, PassengerArrivalRate, RailStationId,
        UtcSeconds,
    },
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        time::advance_time, world::create_new_game,
    },
    ui::{Shell, ShellAction, capture_rendered_buffer, capture_rendered_cell_colors, theme},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const ORIGIN: RailStationId = RailStationId::new(1);
const DESTINATION: RailStationId = RailStationId::new(2);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/06";

fn journey_fixture(waiting_passengers: u32, settle: bool) -> GameState {
    let fare_rate = MoneyPerKilometre::new(10).expect("fixture fare rate is positive");
    let access_rate = MoneyPerKilometre::new(10).expect("fixture access rate is positive");
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.rules.balance = BalanceConfig::new(fare_rate, access_rate, Money::from_cents(400_000));
    state.player_company.funds = Money::from_cents(400_000);
    for pool in &mut state.origin_destination_demand {
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour =
            PassengerArrivalRate::new(1).expect("fixture demand rate is positive");
        pool.market_maturity = MarketMaturity::full();
        pool.fractional_passenger_seconds = 0;
    }
    state
        .origin_destination_demand
        .iter_mut()
        .find(|pool| pool.origin_station_id == ORIGIN && pool.destination_station_id == DESTINATION)
        .expect("fixture route exists")
        .waiting_passengers = waiting_passengers;

    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("fixture purchase succeeds");
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("fixture service exists");
    dispatch_journey(&mut state, train_id, service_id, STARTED_AT)
        .expect("fixture Journey departs");
    if settle {
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).expect("fixture Journey settles");
    }
    state
}

fn finance_fixture(waiting_passengers: u32) -> GameState {
    journey_fixture(waiting_passengers, true)
}

fn company_shell(state: &GameState) -> Shell {
    let mut shell = Shell::new();
    assert_eq!(
        shell.handle_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE), state,),
        ShellAction::Continue
    );
    shell
}

#[test]
fn captures_profitable_and_loss_making_finances_at_wide_and_compact_sizes()
-> Result<(), Box<dyn Error>> {
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let fixtures = [
        (
            "profitable",
            finance_fixture(10),
            "$1,004.50",
            "$10.00",
            "+$4.50",
            "OPERATING",
        ),
        (
            "loss-making",
            finance_fixture(1),
            "$995.50",
            "$1.00",
            "-$4.50",
            "OPERATING",
        ),
    ];

    for (slug, state, funds, revenue, result, status) in fixtures {
        let shell = company_shell(&state);
        for (columns, rows, size) in [(120, 40, "120x40"), (80, 24, "80x24")] {
            let rendered = capture_rendered_buffer(&shell, &state, columns, rows);
            assert_eq!(rendered.lines().count(), rows as usize);
            assert!(rendered.contains(funds), "{slug} should show funds {funds}");
            if columns >= 100 {
                assert!(
                    rendered.contains("Cash"),
                    "{slug} should label available cash in the wide shell header"
                );
            }
            assert!(
                rendered.contains("Fleet value") || rendered.contains("OPERATIONS"),
                "{slug} should show Fleet scale/value context"
            );
            if columns >= 100 {
                assert!(
                    rendered.contains("OPERATIONS"),
                    "{slug} should show the compact operations panel"
                );
                assert!(
                    rendered.contains("Fleet"),
                    "{slug} should show Fleet scale and value"
                );
                assert!(
                    rendered.contains("Services"),
                    "{slug} should show active service ratio"
                );
                assert!(
                    rendered.contains("Network"),
                    "{slug} should show network coverage"
                );
                assert!(
                    rendered.contains("Running"),
                    "{slug} should show live Journey count"
                );
                assert!(
                    rendered.contains("pending revenue"),
                    "{slug} should distinguish booked uncredited revenue from cash"
                );
                assert!(
                    rendered.contains("%"),
                    "{slug} should show network coverage percentage"
                );
                assert!(
                    rendered.contains("Rail "),
                    "{slug} should show compact Company identity"
                );
                assert!(
                    rendered.contains("RECENT JOURNEY RESULTS"),
                    "{slug} should show the recent result chart"
                );
                assert!(
                    rendered.contains("RECENT RESULT"),
                    "{slug} should show recent result as a primary KPI"
                );
                assert!(
                    rendered.contains("RECENT REVENUE"),
                    "{slug} should show recent revenue as a primary KPI"
                );
                let financial_row = rendered
                    .lines()
                    .position(|line| line.contains("RECENT RESULT"))
                    .expect("wide Company dashboard should show KPI cards");
                let operations_row = rendered
                    .lines()
                    .position(|line| line.contains("OPERATIONS"))
                    .expect("wide Company dashboard should show Operations summary");
                assert!(
                    financial_row < operations_row,
                    "{slug} should prioritize KPI cards before operating footprint"
                );
                assert!(
                    rendered.contains("last journey"),
                    "{slug} should label the recent KPI window"
                );
                let recent_outcome_mix = if slug == "profitable" {
                    "1 profitable · 0 losses"
                } else {
                    "0 profitable · 1 loss"
                };
                assert!(
                    rendered.contains(recent_outcome_mix),
                    "{slug} should summarize recent profitable/loss-making Journeys"
                );
                let recent_passengers = if slug == "profitable" { "10 boardings" } else { "1 boarding" };
                assert!(
                    rendered.contains(recent_passengers),
                    "{slug} should summarize recent passenger boardings"
                );
            }
            assert!(
                rendered.contains("$3,000.00"),
                "{slug} should show Fleet value amount"
            );
            assert!(
                rendered.contains("Revenue") || rendered.contains("REVENUE"),
                "{slug} should show revenue"
            );
            assert!(
                rendered.contains(revenue),
                "{slug} should show revenue amount {revenue}"
            );
            assert!(
                rendered.contains("Operating costs")
                    || rendered.contains("OPERATING COSTS")
                    || rendered.contains("RECENT COSTS")
                    || rendered.contains("Total costs"),
                "{slug} should show the operating cost total"
            );
            assert!(
                rendered.contains("$5.50"),
                "{slug} should show the combined operating costs"
            );
            assert!(
                rendered.contains("Operating result") || rendered.contains("OPERATING RESULT") || rendered.contains("RECENT RESULT"),
                "{slug} should label the operating result"
            );
            assert!(
                rendered.contains(result),
                "{slug} should show signed result {result}"
            );
            let margin = if slug == "profitable" {
                "+45.0%"
            } else {
                "-450.0%"
            };
            assert!(
                rendered.contains("Margin") || rendered.contains("MARGIN"),
                "{slug} should show operating margin"
            );
            assert!(
                rendered.contains(margin),
                "{slug} should show operating margin {margin}"
            );
            assert!(
                rendered.contains(status),
                "{slug} should show status {status}"
            );
            fs::write(evidence_dir.join(format!("{slug}-{size}.txt")), rendered)?;
        }
    }

    let wide = company_shell(&fixtures_state_for_insolvency());
    let insolvency = capture_rendered_buffer(&wide, &fixtures_state_for_insolvency(), 120, 40);
    assert!(insolvency.contains("[!] INSOLVENT"));
    assert!(insolvency.contains("Concrete recovery options"));
    fs::write(evidence_dir.join("insolvency-120x40.txt"), insolvency)?;

    let mut bankruptcy_state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    bankruptcy_state.player_company.funds = Money::ZERO;
    let bankruptcy_shell = company_shell(&bankruptcy_state);
    let bankruptcy = capture_rendered_buffer(&bankruptcy_shell, &bankruptcy_state, 120, 40);
    assert!(bankruptcy.contains("BANKRUPTCY"));
    fs::write(evidence_dir.join("bankruptcy-120x40.txt"), bankruptcy)?;

    let insolvency_state = fixtures_state_for_insolvency();
    let insolvency_shell = company_shell(&insolvency_state);
    let insolvency_rendered =
        capture_rendered_buffer(&insolvency_shell, &insolvency_state, 120, 40);
    let (status_x, status_y) = text_position(&insolvency_rendered, "[!] INSOLVENT")
        .expect("insolvency status should be rendered");
    let colors = capture_rendered_cell_colors(
        &insolvency_shell,
        &insolvency_state,
        120,
        40,
        status_x,
        status_y,
    )
    .expect("status section should paint a cell");
    assert_eq!(colors.1, theme::PANEL);
    assert_eq!(colors.0, theme::WARNING);
    Ok(())
}

#[test]
fn wide_dashboard_separates_live_revenue_exposure_from_settled_performance() {
    let state = journey_fixture(10, false);
    let shell = company_shell(&state);
    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);

    assert!(rendered.contains("Running"));
    assert!(rendered.contains("1 train · 10 aboard"));
    assert!(rendered.contains("pending revenue"));
    assert!(rendered.contains("$10.00"));
    assert!(rendered.contains("RECENT JOURNEY RESULTS"));
    assert!(rendered.contains("No completed Journeys yet."));
}

#[test]
fn company_footer_owns_contextual_actions_without_repeating_navigation() {
    let state = finance_fixture(10);
    let mut shell = company_shell(&state);
    let operating = capture_rendered_buffer(&shell, &state, 120, 40);

    assert!(operating.contains("[V] Edit VKM"));
    assert!(operating.contains("[R] Recovery"));
    assert!(!operating.contains("[1] Map"));
    assert!(!operating.contains("[1–4] Navigate"));

    assert_eq!(
        shell.handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &state
        ),
        ShellAction::Continue
    );
    let still_operating = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(!still_operating.contains("Finite recovery routes"));

    let insolvency = fixtures_state_for_insolvency();
    let mut recovery_shell = company_shell(&insolvency);
    assert_eq!(
        recovery_shell.handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &insolvency,
        ),
        ShellAction::Continue
    );
    let recovery = capture_rendered_buffer(&recovery_shell, &insolvency, 120, 40);
    assert!(recovery.contains("Financial Recovery"));
    assert!(recovery.contains("FINITE RECOVERY ROUTES"));
    assert!(recovery.contains("ROUTE DETAILS"));
    assert!(recovery.contains("[Enter] review"));
    assert!(recovery.contains("[Esc] cancel"));
    assert_eq!(
        capture_rendered_cell_colors(&recovery_shell, &insolvency, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Financial Recovery should mute the Company dashboard underneath it",
    );
    assert!(!recovery.contains("Enter opens"));
    assert!(!recovery.contains("Esc returns to Company"));
}

fn fixtures_state_for_insolvency() -> GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    purchase_train(&mut state, 0, ORIGIN).expect("fixture purchase succeeds");
    purchase_train(&mut state, 0, ORIGIN).expect("fixture purchase succeeds");
    state.player_company.funds = Money::ZERO;
    state
}

fn text_position(rendered: &str, needle: &str) -> Option<(u16, u16)> {
    rendered.lines().enumerate().find_map(|(row, line)| {
        line.find(needle).map(|byte_index| {
            let column = line[..byte_index].chars().count();
            (column as u16, row as u16)
        })
    })
}

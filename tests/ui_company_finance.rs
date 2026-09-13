//! Deterministic visual evidence for task 06's Company finance dashboard.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    balance::BalanceConfig,
    model::{GameState, Money, MoneyPerKilometre, PassengerArrivalRate, RailStationId, UtcSeconds},
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

fn finance_fixture(waiting_passengers: u32) -> GameState {
    let fare_rate = MoneyPerKilometre::new(10).expect("fixture fare rate is positive");
    let access_rate = MoneyPerKilometre::new(10).expect("fixture access rate is positive");
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.rules.balance = BalanceConfig::new(fare_rate, access_rate, Money::from_cents(400_000));
    state.player_company.funds = Money::from_cents(400_000);
    for pool in &mut state.origin_destination_demand {
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour =
            PassengerArrivalRate::new(1).expect("fixture demand rate is positive");
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
    let arrives_at = state.active_journeys[0].arrives_at;
    advance_time(&mut state, arrives_at).expect("fixture Journey settles");
    state
}

fn company_shell(state: &GameState) -> Shell {
    let mut shell = Shell::new();
    assert_eq!(
        shell.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), state,),
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
                    rendered.contains("Company Funds"),
                    "{slug} should show Company Funds in the wide shell header"
                );
            } else {
                assert!(rendered.contains("Funds"), "{slug} should show funds in the shell header");
            }
            assert!(
                rendered.contains("Fleet value"),
                "{slug} should show Fleet value"
            );
            if columns >= 100 {
                assert!(rendered.contains("FLEET"), "{slug} should show Fleet section");
                assert!(
                    rendered.contains("SERVICES"),
                    "{slug} should show Services summary"
                );
                assert!(
                    rendered.contains("NETWORK FOOTPRINT"),
                    "{slug} should show network footprint"
                );
                assert!(rendered.contains("Defined"), "{slug} should show defined services");
                assert!(rendered.contains("Active"), "{slug} should show active services");
                assert!(rendered.contains("Idle"), "{slug} should show idle services");
                assert!(rendered.contains("Served"), "{slug} should show served settlements");
                assert!(rendered.contains("Connected"), "{slug} should show connected settlements");
                assert!(rendered.contains("Coverage"), "{slug} should show network coverage");
                assert!(
                    rendered.contains("COMPANY IDENTITY"),
                    "{slug} should show Company identity section"
                );
            }
            assert!(
                rendered.contains("$3,000.00"),
                "{slug} should show Fleet value amount"
            );
            assert!(rendered.contains("Revenue"), "{slug} should show revenue");
            assert!(
                rendered.contains(revenue),
                "{slug} should show revenue amount {revenue}"
            );
            assert!(
                rendered.contains("Operating costs") || rendered.contains("Total costs"),
                "{slug} should show the operating cost total"
            );
            assert!(
                rendered.contains("$5.50"),
                "{slug} should show the combined operating costs"
            );
            if columns >= 100 {
                assert!(
                    rendered.contains("COST BREAKDOWN"),
                    "{slug} should show the cost breakdown"
                );
                assert!(
                    rendered.contains("Access fees"),
                    "{slug} should show access fees"
                );
                assert!(
                    rendered.contains("$1.00"),
                    "{slug} should show access fee amount"
                );
                assert!(rendered.contains("Fuel"), "{slug} should show fuel costs");
                assert!(rendered.contains("$4.50"), "{slug} should show fuel amount");
            }
            assert!(
                rendered.contains("Operating result"),
                "{slug} should label the operating result"
            );
            assert!(
                rendered.contains(result),
                "{slug} should show signed result {result}"
            );
            let margin = if slug == "profitable" { "+45.0%" } else { "-450.0%" };
            assert!(rendered.contains("Margin"), "{slug} should show operating margin");
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
    let insolvency_rendered = capture_rendered_buffer(&insolvency_shell, &insolvency_state, 120, 40);
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

//! Rendered-buffer and keyboard coverage for the Train purchase flow.

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
    catalog::train_catalogue,
    model::{
        GameState, Money, RailStation, RailStationId, Settlement, SettlementId, TrainStatus,
        UtcSeconds,
    },
    sim::{fleet::purchase_train, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer_mut, capture_rendered_cell_colors,
        market::MarketFlow, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/18";
const PURCHASE_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/19";

#[derive(Clone, Debug, Default)]
struct RejectingStore {
    saved: Rc<RefCell<Option<GameState>>>,
    reject_next_save: Rc<Cell<bool>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn delivery_station_list_preserves_model_choice_and_reaches_existing_review()
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
        "Market",
        "Helvetra R70",
        "Veltrian D121",
        "$3,000.00",
        "$4,400.00",
        "IDENTITY",
        "OWNERSHIP",
        "Owned: 0",
        "Ready: 0",
        "Travelling: 0",
        "CAPACITY",
        "PERFORMANCE",
        "ECONOMICS",
        "RESERVE",
        "70 passengers",
        "118.8 km/h",
        "Diesel",
        "$0.38/km",
        "Cash after",
    ] {
        assert!(
            wide.contains(expected),
            "wide catalogue should show {expected}"
        );
    }
    let economics = wide
        .find("ECONOMICS")
        .expect("economics section is visible");
    let capacity = wide.find("CAPACITY").expect("capacity section is visible");
    let performance = wide
        .find("PERFORMANCE")
        .expect("performance section is visible");
    let ownership = wide
        .find("OWNERSHIP")
        .expect("ownership section is visible");
    let identity = wide.find("IDENTITY").expect("identity section is visible");
    assert!(
        economics < capacity
            && capacity < performance
            && performance < ownership
            && ownership < identity,
        "the Market inspector should lead with buying information and leave registration metadata last"
    );
    assert!(
        !wide.contains("Train Market ·"),
        "the workspace frame should own a simple Market title"
    );
    assert!(
        !wide.contains("Purchase decision"),
        "the selected model inspector should not be a second competing panel"
    );
    assert!(
        !wide.contains("Enter · choose delivery Rail Station"),
        "purchase actions belong in the global footer"
    );
    assert!(wide.contains("[Enter] Buy"));
    assert!(!wide.contains("Choose delivery"));
    let (buy_row, buy_column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("[Enter] Buy").map(|column| (row, column)))
        .expect("affordable Market model exposes Buy in the footer");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(buy_column)?,
            u16::try_from(buy_row)?,
        ),
        Some((theme::ACCENT, theme::PANEL)),
    );

    let comparison = capture_rendered_buffer_mut(&mut shell, &state, 160, 40);
    for expected in [
        "Seats",
        "Top speed",
        "Propulsion",
        "Fuel/km",
        "Owned",
        "Status",
        "AFFORDABLE",
    ] {
        assert!(
            comparison.contains(expected),
            "wide catalogue should expose comparison column {expected}"
        );
    }
    assert!(comparison.contains("Diesel"));

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for expected in [
        "Market",
        "Helvetra R70",
        "Veltrian D121",
        "$3,000.00",
        "$4,400.00",
        "70 passengers",
        "118.8 km/h",
        "$0.38/km",
        "EVN type",
    ] {
        assert!(
            compact.contains(expected),
            "80x24 catalogue should retain {expected}"
        );
    }

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    let selected = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    let (row, column) = selected
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("› Veltrian D121").map(|column| (row, column)))
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
    for expected in [
        "1 Train → 2 Delivery Rail Station → 3 Review",
        "Delivery Rail Stations",
        "Selected delivery",
        "Veltrian D121",
        "Left / Backspace · model",
    ] {
        assert!(
            delivery.contains(expected),
            "delivery view should show {expected}"
        );
    }
    for station in &state.region.rail_authority.rail_network.rail_stations {
        let name = state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == station.settlement_id)
            .expect("generated Rail Stations have Settlements")
            .name
            .as_str();
        assert!(delivery.contains(name), "delivery list should show {name}");
    }
    for settlement in &state.region.settlements {
        let has_rail_station = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .any(|station| station.settlement_id == settlement.id);
        if !has_rail_station {
            assert!(
                !delivery.contains(&settlement.name),
                "unconnected Settlement {} must not be a delivery choice",
                settlement.name
            );
        }
    }
    let (row, column) = delivery
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Pinewatch").map(|column| (row, column)))
        .expect("the selected delivery Rail Station is visibly marked");
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
    fs::write(evidence_dir.join("delivery-stations-120x40.txt"), &delivery)?;

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for expected in [
        "Delivery Rail Stations",
        "1 Train → 2 Delivery Rail Station → 3 Review",
        "[Enter] Review",
        "[←] Model",
    ] {
        assert!(
            compact.contains(expected),
            "compact delivery view should show {expected}"
        );
    }
    assert!(!compact.contains("Enter · review"));
    assert!(!compact.contains("Left · model"));
    fs::write(evidence_dir.join("delivery-stations-80x24.txt"), compact)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("Veltrian D121"));
    assert!(review.contains("Confirm Train Purchase"));
    assert!(review.contains("[Enter] confirm"));
    fs::write(evidence_dir.join("delivery-review-120x40.txt"), review)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Backspace),
        ShellAction::Continue
    );
    let restored = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored.contains("› Veltrian D121"));
    assert_eq!(
        state, before,
        "delivery selection, review, and cancellation are presentation-only"
    );
    Ok(())
}

#[test]
fn market_footer_keeps_unaffordable_buy_visible_but_disabled() {
    let mut state = create_new_game(42, "Budget Passenger", STARTED_AT);
    state.player_company.funds = Money::ZERO;
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("UNAFFORDABLE"));
    assert!(rendered.contains("Company Funds are below this purchase price."));
    assert!(rendered.contains("Shortfall"));
    assert!(rendered.contains("[Enter] Buy"));
    let (row, column) = rendered
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("[Enter] Buy").map(|column| (row, column)))
        .expect("unaffordable model keeps Buy visible in the footer");
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

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let rejected = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rejected.contains("Insufficient Company Funds for the selected Train."));
    assert!(!rejected.contains("Delivery Rail Stations"));
}

#[test]
fn market_inspector_summarizes_owned_units_for_the_selected_model() {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(rendered.contains("OWNERSHIP"));
    assert!(rendered.contains("Owned: 1"));
    assert!(rendered.contains("Ready: 1"));
    assert!(rendered.contains("Travelling: 0"));
}

#[test]
fn purchase_review_shows_reserve_consequences_and_commits_only_after_save()
-> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let price = train_catalogue().models()[0].purchase_price();
    purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    state.player_company.funds = price.checked_add(Money::from_cents(500)).unwrap();
    let store = RejectingStore::default();
    let mut app = App::start_new(store.clone(), state).unwrap();
    let mut shell = Shell::new();
    let evidence_dir = Path::new(PURCHASE_EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;

    assert_eq!(
        press(&mut shell, app.state(), KeyCode::Char('b')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, app.state(), KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, app.state(), KeyCode::Enter),
        ShellAction::Continue
    );

    let review = capture_rendered_buffer_mut(&mut shell, app.state(), 120, 40);
    fs::write(evidence_dir.join("purchase-review-120x40.txt"), &review)?;
    for expected in [
        "TRAIN ✓   DELIVERY ✓   REVIEW ●",
        "Confirm Train Purchase",
        "Helvetra R70",
        "TRAIN",
        "EVN type",
        "Capacity",
        "Top speed",
        "Propulsion",
        "DELIVERY",
        "Station",
        "Delivery fee",
        "FINANCIAL",
        "Purchase price",
        "$3,000.00",
        "Cash after",
        "$5.00",
        "RESERVE CHECK",
        "Sample route",
        "Departure cost",
        "After sample",
        "LOW RESERVE",
        "[Enter] confirm",
        "[←] delivery",
        "[Esc] cancel",
    ] {
        assert!(review.contains(expected), "review should show {expected}");
    }
    let (warning_row, warning_column) = review
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("LOW RESERVE").map(|column| (row, column)))
        .expect("the low reserve warning should be visible beside confirmation");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            app.state(),
            120,
            40,
            u16::try_from(warning_column)?,
            u16::try_from(warning_row)?,
        ),
        Some((theme::WARNING, theme::PANEL))
    );
    let compact = capture_rendered_buffer_mut(&mut shell, app.state(), 80, 24);
    for expected in [
        "Helvetra R70",
        "Delivery",
        "Price",
        "$3,000.00",
        "Cash after",
        "$5.00",
        "Reserve sample",
        "LOW RESERVE",
        "[Enter] confirm",
    ] {
        assert!(
            compact.contains(expected),
            "compact review should show {expected}"
        );
    }
    fs::write(evidence_dir.join("purchase-review-80x24.txt"), compact)?;

    assert_eq!(
        press(&mut shell, app.state(), KeyCode::Left),
        ShellAction::Continue
    );
    let returned = capture_rendered_buffer_mut(&mut shell, app.state(), 120, 40);
    assert!(returned.contains("Delivery Rail Stations"));
    assert!(returned.contains("> Pinewatch"));
    assert_eq!(
        press(&mut shell, app.state(), KeyCode::Enter),
        ShellAction::Continue
    );

    let before_rejection = app.state().clone();
    store.reject_next_save.set(true);
    let rejected_action = press(&mut shell, app.state(), KeyCode::Enter);
    let ShellAction::PurchaseTrain {
        catalogue_index,
        delivery_station_id,
    } = rejected_action
    else {
        panic!("only explicit review confirmation may request a purchase");
    };
    let rejection = app
        .purchase_train(catalogue_index, delivery_station_id, STARTED_AT)
        .unwrap_err();
    shell.reject_purchase_train(rejection.to_string());
    assert_eq!(
        app.state(),
        &before_rejection,
        "failed save must not publish a Train"
    );
    let rejected = capture_rendered_buffer_mut(&mut shell, app.state(), 120, 40);
    assert!(rejected.contains("Purchase rejected: could not save game changes"));
    assert!(rejected.contains("LOW RESERVE"));
    assert!(!rejected.contains("authorised and saved"));
    fs::write(
        evidence_dir.join("purchase-save-rejected-120x40.txt"),
        rejected,
    )?;

    let confirmed_action = press(&mut shell, app.state(), KeyCode::Enter);
    assert_eq!(
        confirmed_action,
        ShellAction::PurchaseTrain {
            catalogue_index,
            delivery_station_id,
        }
    );
    let train_id = app
        .purchase_train(catalogue_index, delivery_station_id, STARTED_AT)
        .unwrap();
    shell.confirm_purchase_train();
    assert_eq!(app.state().player_company.fleet.trains.len(), 2);
    assert_eq!(app.state().player_company.funds, Money::from_cents(500));
    let purchased_train = app.state().player_company.fleet.trains.last().unwrap();
    assert_eq!(purchased_train.id, train_id);
    assert_eq!(
        purchased_train.status,
        TrainStatus::Ready {
            at: delivery_station_id
        }
    );
    assert_eq!(store.load().unwrap(), Some(app.state().clone()));
    Ok(())
}

#[test]
fn delivery_selection_scrolls_and_recovers_when_its_station_disappears() {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    for index in 5_u64..=24 {
        let settlement_id = SettlementId::new(index + 100);
        state.region.settlements.push(Settlement {
            id: settlement_id,
            name: format!("Long Delivery Station {index}"),
            population: 1_000,
            position: crate::model::WorldPosition::default(),
        });
        state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .push(RailStation {
                id: RailStationId::new(index + 100),
                settlement_id,
            });
    }
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let _ = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let scrolled = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(
        scrolled.contains("> Long Delivery Station 24"),
        "the selected delivery Rail Station must scroll into view"
    );

    let mut stale_state = create_new_game(42, "Stale Delivery", STARTED_AT);
    let mut stale_shell = Shell::new();
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Enter),
        ShellAction::Continue
    );
    stale_state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .remove(0);
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Enter),
        ShellAction::Continue
    );
    let stale = capture_rendered_buffer_mut(&mut stale_shell, &stale_state, 120, 40);
    assert!(stale.contains("previously selected delivery Rail Station is no longer available"));
    assert!(stale.contains("Delivery Rail Stations"));

    let mut missing_state = create_new_game(42, "Missing Delivery", STARTED_AT);
    missing_state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .clear();
    assert_eq!(
        MarketFlow::start(&missing_state, 0).unwrap_err(),
        "No connected Rail Station is available for delivery."
    );
}

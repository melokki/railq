//! Visual evidence for the shared amount and operating-unit conventions.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    ui::{Shell, ShellAction, capture_rendered_buffer_mut},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/20";

#[test]
fn captures_consistent_amounts_and_units_across_operating_views() -> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(12_345_678);
    let train_id = purchase_train(&mut state, 0, RailStationId::new(1))?;
    let service = find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))?;
    dispatch_journey(&mut state, train_id, service, STARTED_AT)?;

    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let mut shell = Shell::new();
    for (key, name) in [
        (None, "map-120x40.txt"),
        (Some(KeyCode::Char('T')), "fleet-120x40.txt"),
        (Some(KeyCode::Char('4')), "company-120x40.txt"),
        (Some(KeyCode::Char('B')), "market-120x40.txt"),
    ] {
        if let Some(key) = key {
            assert_eq!(
                shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state),
                ShellAction::Continue
            );
        }
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
        assert!(
            rendered.contains("$"),
            "{name} should expose formatted money"
        );
        assert!(
            rendered.contains("km") || rendered.contains("ETA"),
            "{name} should expose operating units"
        );
        fs::write(evidence_dir.join(name), rendered)?;
    }
    Ok(())
}

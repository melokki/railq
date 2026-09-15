mod runtime;

use std::error::Error;

use railq::{
    storage::SaveSlot,
    ui::{
        self,
        start::{Startup, capture_new_game, start},
    },
};

use runtime::{current_utc_seconds, new_world_seed};

fn main() -> Result<(), Box<dyn Error>> {
    let slot = SaveSlot::open_default()?;
    match start(slot, current_utc_seconds())? {
        Startup::Dashboard(dashboard) => {
            let (mut app, settled_arrivals) = dashboard.into_parts();
            run_dashboard(&mut app, settled_arrivals)?;
        }
        Startup::Onboarding(onboarding) => run_onboarding(onboarding)?,
    }
    Ok(())
}

fn run_onboarding(
    onboarding: railq::ui::start::Onboarding<SaveSlot>,
) -> Result<(), Box<dyn Error>> {
    let world_seed = new_world_seed();
    let started_at = current_utc_seconds();
    let Some(game) = capture_new_game(world_seed, started_at)? else {
        return Ok(());
    };

    let mut app = onboarding.save(game)?;
    run_dashboard(&mut app, Vec::new())?;
    Ok(())
}

fn run_dashboard(
    app: &mut railq::app::App<SaveSlot>,
    settled_arrivals: Vec<railq::sim::time::SettledJourney>,
) -> Result<(), Box<dyn Error>> {
    ui::run_terminal_with_arrivals(app.state().clone(), settled_arrivals, |command| {
        let now = current_utc_seconds();
        match command {
            ui::TerminalCommand::Reconcile => app.reconcile(now)?,
            ui::TerminalCommand::Player(command) => {
                app.execute(command, now)?;
            }
            ui::TerminalCommand::RestartAfterBankruptcy => {
                app.restart_after_bankruptcy(new_world_seed(), now)?;
            }
        }
        Ok::<_, railq::app::AppError<railq::storage::SaveSlotError>>(app.state().clone())
    })?;
    Ok(())
}

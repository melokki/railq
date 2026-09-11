use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use railq::{
    model::UtcSeconds,
    storage::SaveSlot,
    ui::{
        self,
        start::{Startup, capture_company_name, onboarding_summary, start},
    },
};

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
    let Some(company_name) = capture_company_name()? else {
        return Ok(());
    };
    let game = onboarding.prepare_company(company_name, startup_seed(), current_utc_seconds());

    println!("\n{}\n", onboarding_summary(&game));
    let mut app = onboarding.save(game)?;
    println!("Player Company saved. Opening dashboard...");
    run_dashboard(&mut app, Vec::new())?;
    Ok(())
}

fn run_dashboard(
    app: &mut railq::app::App<SaveSlot>,
    settled_arrivals: Vec<railq::sim::time::SettledJourney>,
) -> Result<(), Box<dyn Error>> {
    ui::run_terminal_with_arrivals(app.state().clone(), settled_arrivals, |command| {
        match command {
            ui::TerminalCommand::Reconcile { now } => app.reconcile(now)?,
            ui::TerminalCommand::ManualDispatch {
                train_id,
                destination_station_id,
                now,
            } => {
                app.dispatch_to_destination(train_id, destination_station_id, now)?;
            }
            ui::TerminalCommand::PurchaseTrain {
                catalogue_index,
                delivery_station_id,
                now,
            } => {
                app.purchase_train(catalogue_index, delivery_station_id, now)?;
            }
            ui::TerminalCommand::SellTrain { train_id, now } => {
                app.sell_train(train_id, now)?;
            }
            ui::TerminalCommand::RestartAfterBankruptcy { world_seed, now } => {
                app.restart_after_bankruptcy(world_seed, now)?;
            }
        }
        Ok::<_, railq::app::AppError<railq::storage::SaveSlotError>>(app.state().clone())
    })?;
    Ok(())
}

fn current_utc_seconds() -> UtcSeconds {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    UtcSeconds::from_unix_seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn startup_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    nanos ^ u64::from(std::process::id())
}

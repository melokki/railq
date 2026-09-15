use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use railq::{
    model::UtcSeconds,
    storage::SaveSlot,
    ui::{
        self,
        start::{Startup, capture_new_game, start},
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
    let world_seed = startup_seed();
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
        match command {
            ui::TerminalCommand::Reconcile { now } => app.reconcile(now)?,
            ui::TerminalCommand::ManualDispatch {
                train_id,
                service_id,
                now,
            } => {
                app.dispatch_journey(train_id, service_id, now)?;
            }
            ui::TerminalCommand::PurchaseTrain {
                catalogue_index,
                delivery_station_id,
                now,
            } => {
                app.execute(
                    railq::app::AppCommand::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                    },
                    now,
                )?;
            }
            ui::TerminalCommand::SellTrain { train_id, now } => {
                app.execute(railq::app::AppCommand::SellTrain { train_id }, now)?;
            }
            ui::TerminalCommand::CreatePassengerService {
                stop_station_ids,
                now,
            } => {
                app.create_passenger_service(stop_station_ids, now)?;
            }
            ui::TerminalCommand::UpdatePassengerService {
                service_id,
                stop_station_ids,
                now,
            } => {
                app.update_passenger_service(service_id, stop_station_ids, now)?;
            }
            ui::TerminalCommand::DeletePassengerService { service_id, now } => {
                app.delete_passenger_service(service_id, now)?;
            }
            ui::TerminalCommand::UpdateCompanyVkm {
                vehicle_keeper_mark,
                now,
            } => {
                app.execute(
                    railq::app::AppCommand::UpdateCompanyVkm {
                        vehicle_keeper_mark,
                    },
                    now,
                )?;
            }
            ui::TerminalCommand::ContributeInfrastructure {
                project_id,
                amount,
                now,
            } => {
                app.execute(
                    railq::app::AppCommand::ContributeInfrastructure { project_id, amount },
                    now,
                )?;
            }
            ui::TerminalCommand::UpdateTrainNickname {
                train_id,
                nickname,
                now,
            } => {
                app.execute(
                    railq::app::AppCommand::UpdateTrainNickname { train_id, nickname },
                    now,
                )?;
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

use std::{
    error::Error,
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use railq::{
    model::UtcSeconds,
    storage::SaveSlot,
    ui::{
        self,
        start::{CompanyName, Startup, onboarding_summary, start},
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    let slot = SaveSlot::open_default()?;
    match start(slot, current_utc_seconds())? {
        Startup::Dashboard(mut app) => run_dashboard(&mut app)?,
        Startup::Onboarding(onboarding) => run_onboarding(onboarding)?,
    }
    Ok(())
}

fn run_onboarding(
    onboarding: railq::ui::start::Onboarding<SaveSlot>,
) -> Result<(), Box<dyn Error>> {
    println!("Welcome to RailQ. Name your Player Company.");
    let mut company_name = String::new();
    let game = loop {
        print!("Player Company name: ");
        io::stdout().flush()?;
        company_name.clear();
        if io::stdin().read_line(&mut company_name)? == 0 {
            return Err("onboarding ended before a Player Company name was entered".into());
        }
        match CompanyName::parse(&company_name) {
            Ok(name) => {
                break onboarding.prepare_company(name, startup_seed(), current_utc_seconds());
            }
            Err(error) => println!("{error}"),
        }
    };

    println!("\n{}\n", onboarding_summary(&game));
    let mut app = onboarding.save(game)?;
    println!("Player Company saved. Opening dashboard...");
    run_dashboard(&mut app)?;
    Ok(())
}

fn run_dashboard(app: &mut railq::app::App<SaveSlot>) -> Result<(), Box<dyn Error>> {
    ui::run_terminal(app.state().clone(), |now| {
        app.reconcile(now)?;
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

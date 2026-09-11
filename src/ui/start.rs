//! Startup and onboarding for a Player Company.
//!
//! This module decides whether a save opens the dashboard or begins
//! onboarding. Terminal input and output remain at the binary boundary, while
//! these types keep startup behaviour deterministic and testable.

use std::{error::Error, fmt};

use crate::{
    app::{App, AppError, GameStore},
    model::{GameState, UtcSeconds},
    sim::time::SettledJourney,
    sim::world::create_new_game,
};

/// The maximum visible length of a Player Company name.
pub const MAXIMUM_COMPANY_NAME_CHARACTERS: usize = 60;

/// A validated Player Company name suitable for starting a new game.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompanyName(String);

impl CompanyName {
    /// Validates and normalizes a player-entered Player Company name.
    pub fn parse(value: &str) -> Result<Self, CompanyNameError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(CompanyNameError::Empty);
        }
        if value.chars().count() > MAXIMUM_COMPANY_NAME_CHARACTERS {
            return Err(CompanyNameError::TooLong {
                maximum: MAXIMUM_COMPANY_NAME_CHARACTERS,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(CompanyNameError::ContainsControlCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the normalized Player Company name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a new Player Company name cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompanyNameError {
    /// A name must include at least one non-whitespace character.
    Empty,
    /// Names must fit in the onboarding screen.
    TooLong { maximum: usize },
    /// Control characters would make terminal output ambiguous.
    ContainsControlCharacter,
}

impl fmt::Display for CompanyNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "Enter a Player Company name."),
            Self::TooLong { maximum } => {
                write!(
                    formatter,
                    "Use at most {maximum} characters for the Player Company name."
                )
            }
            Self::ContainsControlCharacter => {
                write!(
                    formatter,
                    "The Player Company name cannot contain control characters."
                )
            }
        }
    }
}

impl Error for CompanyNameError {}

/// The startup destination selected from the local save slot.
#[derive(Debug)]
pub enum Startup<S> {
    /// A validated Player Company is ready for the terminal dashboard.
    Dashboard(StartupDashboard<S>),
    /// No save exists, so the player must name a new Player Company.
    Onboarding(Onboarding<S>),
}

/// The initial dashboard application and arrivals committed while loading.
#[derive(Debug)]
pub struct StartupDashboard<S> {
    app: Box<App<S>>,
    settled_arrivals: Vec<SettledJourney>,
}

impl<S> StartupDashboard<S> {
    /// Consumes the one-time startup handoff before entering the shell.
    pub fn into_parts(self) -> (Box<App<S>>, Vec<SettledJourney>) {
        (self.app, self.settled_arrivals)
    }
}

/// An exclusively owned empty save slot awaiting a new Player Company.
#[derive(Debug)]
pub struct Onboarding<S> {
    store: S,
}

impl<S: GameStore> Onboarding<S> {
    /// Builds a named game so its Region and Concession can be shown before it
    /// is persisted. The caller must use [`Self::save`] before opening play.
    pub fn prepare_company(
        &self,
        company_name: CompanyName,
        world_seed: u64,
        started_at: UtcSeconds,
    ) -> GameState {
        create_new_game(world_seed, company_name.0, started_at)
    }

    /// Persists the prepared Player Company before returning the dashboard.
    pub fn save(self, state: GameState) -> Result<App<S>, StartupError<S::Error>> {
        App::start_new(self.store, state).map_err(StartupError::Save)
    }
}

/// Loads an existing Player Company, or starts onboarding only when no save
/// exists. A malformed save is never treated as a missing save.
pub fn start<S: GameStore>(
    store: S,
    now: UtcSeconds,
) -> Result<Startup<S>, StartupError<S::Error>> {
    match App::load_or_empty(store, now).map_err(StartupError::Load)? {
        Ok(loaded) => {
            let (app, settled_arrivals) = loaded.into_parts();
            Ok(Startup::Dashboard(StartupDashboard {
                app: Box::new(app),
                settled_arrivals,
            }))
        }
        Err(store) => Ok(Startup::Onboarding(Onboarding { store })),
    }
}

/// A startup failure with recovery guidance for the player.
#[derive(Debug)]
pub enum StartupError<E> {
    /// An existing save could not be loaded or reconciled.
    Load(AppError<E>),
    /// A new Player Company could not be persisted.
    Save(AppError<E>),
}

impl<E: fmt::Display> fmt::Display for StartupError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(
                formatter,
                "Could not load the existing Player Company: {error}. The save was not reset or overwritten. Repair or move the save aside, then restart RailQ."
            ),
            Self::Save(error) => write!(
                formatter,
                "Could not save the new Player Company: {error}. Resolve the storage problem and try again; the dashboard has not opened."
            ),
        }
    }
}

impl<E: Error + 'static> Error for StartupError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) | Self::Save(error) => Some(error),
        }
    }
}

/// Formats the generated Region and Concession shown during onboarding.
pub fn onboarding_summary(state: &GameState) -> String {
    format!(
        "Region: {}\nPopulation: {}\nConnected settlements: {}\n\nConcession: {} has awarded {} the right to operate passenger railway services over the public Rail Network.\nCompany Funds: {} cents",
        state.region.name,
        state.region.population,
        state.region.rail_authority.rail_network.rail_stations.len(),
        state.region.rail_authority.name,
        state.player_company.name,
        state.player_company.funds.cents(),
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, error::Error, fmt, rc::Rc};

    use crate::{
        app::GameStore,
        model::{GameState, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{CompanyName, CompanyNameError, Startup, start};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

    #[derive(Clone, Debug, Default)]
    struct TestStore {
        saved: Rc<RefCell<Option<GameState>>>,
        load_error: Rc<RefCell<Option<TestStoreError>>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestStoreError {
        MalformedSave,
    }

    impl fmt::Display for TestStoreError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "save is malformed and was preserved")
        }
    }

    impl Error for TestStoreError {}

    impl GameStore for TestStore {
        type Error = TestStoreError;

        fn load(&self) -> Result<Option<GameState>, Self::Error> {
            match *self.load_error.borrow() {
                Some(error) => Err(error),
                None => Ok(self.saved.borrow().clone()),
            }
        }

        fn save(&self, state: &GameState) -> Result<(), Self::Error> {
            self.saved.replace(Some(state.clone()));
            Ok(())
        }
    }

    #[test]
    fn company_name_is_trimmed_and_rejects_invalid_input() {
        assert_eq!(
            CompanyName::parse("  Alden Passenger  ").unwrap().as_str(),
            "Alden Passenger"
        );
        assert_eq!(CompanyName::parse("  "), Err(CompanyNameError::Empty));
        assert_eq!(
            CompanyName::parse("A\nB"),
            Err(CompanyNameError::ContainsControlCharacter)
        );
        assert!(matches!(
            CompanyName::parse(&"A".repeat(61)),
            Err(CompanyNameError::TooLong { maximum: 60 })
        ));
    }

    #[test]
    fn onboarding_shows_region_and_concession_then_persists_before_dashboard() {
        let store = TestStore::default();
        let Startup::Onboarding(onboarding) = start(store.clone(), STARTED_AT).unwrap() else {
            panic!("a missing save starts onboarding");
        };
        let game = onboarding.prepare_company(
            CompanyName::parse("Alden Passenger").unwrap(),
            42,
            STARTED_AT,
        );
        let summary = super::onboarding_summary(&game);

        assert!(summary.contains(&format!("Region: {}", game.region.name)));
        assert!(summary.contains("Concession:"));
        assert!(store.saved.borrow().is_none());

        let app = onboarding.save(game.clone()).unwrap();
        assert_eq!(app.state(), &game);
        assert_eq!(*store.saved.borrow(), Some(game));
    }

    #[test]
    fn an_existing_player_company_loads_directly_to_dashboard() {
        let saved_game = create_new_game(42, "Existing Passenger", STARTED_AT);
        let store = TestStore {
            saved: Rc::new(RefCell::new(Some(saved_game.clone()))),
            ..TestStore::default()
        };

        let Startup::Dashboard(app) = start(store, STARTED_AT).unwrap() else {
            panic!("an existing save opens the dashboard");
        };
        let (app, settled_arrivals) = app.into_parts();
        assert_eq!(app.state(), &saved_game);
        assert!(settled_arrivals.is_empty());
    }

    #[test]
    fn malformed_save_has_recovery_guidance_and_never_starts_onboarding() {
        let store = TestStore {
            load_error: Rc::new(RefCell::new(Some(TestStoreError::MalformedSave))),
            ..TestStore::default()
        };

        let error = start(store, STARTED_AT).unwrap_err().to_string();
        assert!(error.contains("save is malformed and was preserved"));
        assert!(error.contains("not reset or overwritten"));
        assert!(error.contains("Repair or move the save aside"));
    }
}

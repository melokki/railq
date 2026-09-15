//! SQLite persistence for RailQ.
//!
//! Mutable game state is stored in normalized tables rather than one growing
//! RON document. The application still works with a validated [`GameState`]
//! at its boundary; this module is responsible for reconstructing that state
//! transactionally and checking every simulation invariant before publishing
//! it to the rest of the program.

mod legacy;
mod migrations;
mod schema;
mod state_io;
mod validation;

pub use self::validation::{SaveValidationError, validate_game_state};

use self::{
    legacy::decode_legacy_game_state,
    migrations::ensure_schema,
    schema::SCHEMA,
    state_io::{clear_state, insert_state, load_state},
};

#[cfg(test)]
use self::migrations::migrate_v9_to_v10;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    hash::Hash,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs4::{FileExt, TryLockError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{
    balance::BalanceConfig,
    catalog::{TrainModel, model_for_train, train_catalogue},
    model::{
        BulletinCategory, BulletinEntry, CalculationError, ConstructionDifficulty, DemandRules,
        DistanceMetres, DurationSeconds, Electrification, EuropeanVehicleNumber, Financials, Fleet,
        GameRules, GameState, InfrastructureProject, InfrastructureProjectFunding,
        InfrastructureProjectId, InfrastructureProjectKind, InfrastructureProjectStatus,
        InfrastructureProjectTimeline, Journey, JourneyId, JourneyPassengerGroup, JourneyReceipt,
        MarketMaturity, Money, MoneyPerKilometre, OriginDestinationDemand, PassengerArrivalRate,
        PassengerService, PlannedRailLine, PlannedRailStation, PlayerCompany, RailAuthority,
        RailAuthorityFinances, RailLine, RailLineId, RailNetwork, RailStation, RailStationId,
        RailwayRegistration, Region, ServiceId, Settlement, SettlementId, SpeedKilometresPerHour,
        TrackCount, Train, TrainId, TrainModelId, TrainNickname, TrainStatus, UtcSeconds,
        VehicleKeeperMark, WorldPosition,
    },
    sim::{
        demand::waiting_passenger_cap,
        services::{path_between_stations, service_path_for_stops},
        world::{
            railway_registration_for_existing_region, settlement_positions_for_existing_region,
        },
    },
};

/// SQLite schema understood by this build.
pub const SAVE_VERSION: u32 = 27;

/// The local SQLite save used when no explicit path is supplied.
pub const DEFAULT_SAVE_PATH: &str = "railq.db";

/// Previous default RON save. `open_default` imports it once when no database
/// exists yet, leaving the source file untouched as a player-visible backup.
pub const LEGACY_SAVE_PATH: &str = "railq.ron";

static NEXT_ARCHIVE_ID: AtomicU64 = AtomicU64::new(0);

/// Number of settled Journey receipts kept in the live in-memory state.
/// SQLite retains the complete history for future statistics and reports.
const RECENT_RECEIPT_LIMIT: usize = 100;

#[derive(Debug)]
pub struct SaveSlot {
    path: PathBuf,
    _lock_file: File,
}

#[derive(Debug)]
pub enum SaveSlotError {
    InvalidPath {
        path: PathBuf,
    },
    AlreadyOwned {
        path: PathBuf,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Database {
        action: &'static str,
        path: PathBuf,
        source: rusqlite::Error,
    },
    InvalidSave {
        path: PathBuf,
        source: Box<SaveCodecError>,
    },
}

impl fmt::Display for SaveSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => {
                write!(formatter, "save path {} has no file name", path.display())
            }
            Self::AlreadyOwned { path } => {
                write!(formatter, "save slot {} is already owned", path.display())
            }
            Self::Io {
                action,
                path,
                source,
            } => {
                write!(formatter, "could not {action} {}: {source}", path.display())
            }
            Self::Database {
                action,
                path,
                source,
            } => {
                write!(
                    formatter,
                    "could not {action} SQLite save {}: {source}",
                    path.display()
                )
            }
            Self::InvalidSave { path, source } => {
                write!(
                    formatter,
                    "save {} is invalid and was preserved: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for SaveSlotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Database { source, .. } => Some(source),
            Self::InvalidSave { source, .. } => Some(source),
            Self::InvalidPath { .. } | Self::AlreadyOwned { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveCodecError {
    UnsupportedVersion { found: u32 },
    InvalidState(SaveValidationError),
    InvalidValue { field: &'static str },
    TrainModelNotFound { model_name: String },
    LegacyDecode(String),
}

impl fmt::Display for SaveCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => {
                write!(formatter, "save version {found} is not supported")
            }
            Self::InvalidState(error) => error.fmt(formatter),
            Self::InvalidValue { field } => write!(formatter, "invalid SQLite value for {field}"),
            Self::TrainModelNotFound { model_name } => write!(
                formatter,
                "saved Train model {model_name:?} does not exist in the central catalogue"
            ),
            Self::LegacyDecode(error) => {
                write!(formatter, "could not decode legacy RON save: {error}")
            }
        }
    }
}

impl Error for SaveCodecError {}

impl SaveSlot {
    pub fn open_default() -> Result<Self, SaveSlotError> {
        let database_existed = Path::new(DEFAULT_SAVE_PATH).exists();
        let slot = Self::open(DEFAULT_SAVE_PATH)?;
        if !database_existed && Path::new(LEGACY_SAVE_PATH).exists() {
            let source =
                fs::read_to_string(LEGACY_SAVE_PATH).map_err(|source| SaveSlotError::Io {
                    action: "read legacy RON save",
                    path: PathBuf::from(LEGACY_SAVE_PATH),
                    source,
                })?;
            let state =
                decode_legacy_game_state(&source).map_err(|source| SaveSlotError::InvalidSave {
                    path: PathBuf::from(LEGACY_SAVE_PATH),
                    source: Box::new(source),
                })?;
            slot.save(&state)?;
        }
        Ok(slot)
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SaveSlotError> {
        let path = path.into();
        let lock_path = sidecar_lock_path(&path)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| SaveSlotError::Io {
                action: "open save lock",
                path: lock_path.clone(),
                source,
            })?;
        match FileExt::try_lock(&lock_file) {
            Ok(()) => Ok(Self {
                path,
                _lock_file: lock_file,
            }),
            Err(TryLockError::WouldBlock) => Err(SaveSlotError::AlreadyOwned { path }),
            Err(TryLockError::Error(source)) => Err(SaveSlotError::Io {
                action: "lock save slot",
                path,
                source,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<GameState>, SaveSlotError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let connection = self.open_connection("open")?;
        ensure_schema(&connection, &self.path)?;
        let state = load_state(&connection, &self.path)?;
        if let Some(state) = &state {
            validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
                path: self.path.clone(),
                source: Box::new(SaveCodecError::InvalidState(source)),
            })?;
        }
        Ok(state)
    }

    pub fn save(&self, state: &GameState) -> Result<(), SaveSlotError> {
        validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(SaveCodecError::InvalidState(source)),
        })?;
        if self.path.exists() {
            self.load()?;
        }
        self.replace_state(state)
    }

    pub fn save_after_backup(&self, state: &GameState) -> Result<PathBuf, SaveSlotError> {
        self.load()?;
        validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(SaveCodecError::InvalidState(source)),
        })?;
        let backup_path = archive_save(&self.path).map_err(|source| SaveSlotError::Io {
            action: "archive existing save before restart",
            path: self.path.clone(),
            source,
        })?;
        self.replace_state(state)?;
        Ok(backup_path)
    }

    fn replace_state(&self, state: &GameState) -> Result<(), SaveSlotError> {
        let mut connection = self.open_connection("open for write")?;
        ensure_schema(&connection, &self.path)?;
        let existing_world_seed = connection
            .query_row(
                "SELECT world_seed FROM game_meta WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| SaveSlotError::Database {
                action: "read existing game identity from",
                path: self.path.clone(),
                source,
            })?;
        let current_world_seed = state.world_seed.to_string();
        let preserve_history = existing_world_seed.as_deref() == Some(current_world_seed.as_str());
        let transaction = connection
            .transaction()
            .map_err(|source| SaveSlotError::Database {
                action: "begin transaction for",
                path: self.path.clone(),
                source,
            })?;
        clear_state(&transaction, &self.path, preserve_history)?;
        insert_state(&transaction, state, &self.path)?;
        transaction
            .commit()
            .map_err(|source| SaveSlotError::Database {
                action: "commit transaction for",
                path: self.path.clone(),
                source,
            })?;
        connection
            .execute_batch("PRAGMA optimize;")
            .map_err(|source| SaveSlotError::Database {
                action: "optimize",
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    fn open_connection(&self, action: &'static str) -> Result<Connection, SaveSlotError> {
        let connection =
            Connection::open(&self.path).map_err(|source| SaveSlotError::Database {
                action,
                path: self.path.clone(),
                source,
            })?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
            .map_err(|source| SaveSlotError::Database {
                action: "configure",
                path: self.path.clone(),
                source,
            })?;
        Ok(connection)
    }
}

fn sidecar_lock_path(path: &Path) -> Result<PathBuf, SaveSlotError> {
    let Some(file_name) = path.file_name() else {
        return Err(SaveSlotError::InvalidPath {
            path: path.to_path_buf(),
        });
    };
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

fn archive_save(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "save path has no file name"))?;
    for _ in 0..128 {
        let sequence = NEXT_ARCHIVE_ID.fetch_add(1, Ordering::Relaxed);
        let archive_path = parent.join(format!(
            "{}.bankrupt-backup-{}.{}.db",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive_path)
        {
            Ok(_) => {
                fs::copy(path, &archive_path)?;
                return Ok(archive_path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique restart backup save",
    ))
}

fn catalogue_model_for_persisted_id(model_id: &str) -> Option<&'static TrainModel> {
    let requested = TrainModelId::new(model_id.to_owned());
    train_catalogue().by_id(&requested).or_else(|| {
        let replacement_id = match model_id {
            "local-70" => "helvetra-r70",
            "express-120" => "veltrian-d121",
            _ => return None,
        };
        train_catalogue().by_id(&TrainModelId::new(replacement_id))
    })
}

fn catalogue_model_for_legacy_signature(
    model_name: &str,
    passenger_capacity: i64,
    speed_metres_per_second: i64,
    fuel_cost_cents_per_kilometre: i64,
) -> Option<&'static TrainModel> {
    train_catalogue()
        .models()
        .iter()
        .find(|model| {
            model.name() == model_name
                && i64::from(model.passenger_capacity().passengers()) == passenger_capacity
                && i64::try_from(model.speed().metres_per_second()).ok()
                    == Some(speed_metres_per_second)
                && i64::try_from(model.fuel_cost_per_kilometre().cents_per_kilometre()).ok()
                    == Some(fuel_cost_cents_per_kilometre)
        })
        .or_else(|| {
            let replacement_id = match (
                model_name,
                passenger_capacity,
                speed_metres_per_second,
                fuel_cost_cents_per_kilometre,
            ) {
                ("Local 70", 70, 25, 45) => "helvetra-r70",
                ("Express 120", 120, 33, 30) => "veltrian-d121",
                _ => return None,
            };
            train_catalogue().by_id(&TrainModelId::new(replacement_id))
        })
}

const V16_IDENTITY_TABLES_COMPAT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS region (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    registration_code INTEGER NOT NULL,
    registration_mark TEXT NOT NULL,
    population INTEGER NOT NULL,
    rail_authority_name TEXT NOT NULL,
    next_infrastructure_project_id INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS settlements (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    population INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rail_stations (
    id INTEGER PRIMARY KEY,
    settlement_id INTEGER NOT NULL REFERENCES settlements(id)
);
CREATE TABLE IF NOT EXISTS rail_lines (
    id INTEGER PRIMARY KEY,
    first_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    second_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    distance_metres INTEGER NOT NULL,
    speed_limit_kmh INTEGER NOT NULL DEFAULT 70,
    track_count INTEGER NOT NULL DEFAULT 1,
    electrification TEXT NOT NULL DEFAULT 'none',
    construction_difficulty TEXT NOT NULL DEFAULT 'moderate'
);
CREATE TABLE IF NOT EXISTS infrastructure_projects (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    review_started_at INTEGER,
    proposed_at INTEGER,
    approved_at INTEGER,
    funding_completed_at INTEGER,
    scheduled_start_at INTEGER,
    construction_started_at INTEGER,
    planned_completion_at INTEGER,
    completed_at INTEGER,
    deferred_at INTEGER,
    cancelled_at INTEGER,
    target_speed_limit_kmh INTEGER,
    target_track_count INTEGER
);
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_lines (
    project_id INTEGER NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id INTEGER NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_stations (
    project_id INTEGER NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_stations (
    project_id INTEGER NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id INTEGER NOT NULL UNIQUE,
    settlement_id INTEGER NOT NULL REFERENCES settlements(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_lines (
    project_id INTEGER NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    line_id INTEGER NOT NULL UNIQUE,
    first_station_id INTEGER NOT NULL,
    second_station_id INTEGER NOT NULL,
    distance_metres INTEGER NOT NULL,
    speed_limit_kmh INTEGER NOT NULL,
    track_count INTEGER NOT NULL,
    electrification TEXT NOT NULL,
    construction_difficulty TEXT NOT NULL,
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS company (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    vkm TEXT NOT NULL DEFAULT 'RQP',
    funds_cents INTEGER NOT NULL,
    next_train_id INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS trains (
    id INTEGER PRIMARY KEY,
    evn TEXT NOT NULL UNIQUE,
    nickname TEXT,
    status_kind TEXT NOT NULL,
    status_ref_id INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    original_purchase_price_cents INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS passenger_services (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS service_stops (
    service_id INTEGER NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS service_lines (
    service_id INTEGER NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id INTEGER NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS origin_destination_demand (
    origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    waiting_passengers INTEGER NOT NULL,
    passenger_arrival_rate_per_hour INTEGER NOT NULL,
    fractional_passenger_seconds INTEGER NOT NULL,
    PRIMARY KEY (origin_station_id, destination_station_id)
);
CREATE TABLE IF NOT EXISTS active_journeys (
    id INTEGER PRIMARY KEY,
    service_id INTEGER NOT NULL REFERENCES passenger_services(id),
    train_id INTEGER NOT NULL REFERENCES trains(id),
    origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    passengers_carried INTEGER NOT NULL,
    fare_cents INTEGER NOT NULL,
    operating_revenue_cents INTEGER NOT NULL,
    credited_revenue_cents INTEGER NOT NULL DEFAULT 0,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    current_stop_index INTEGER NOT NULL DEFAULT 1,
    departed_at INTEGER NOT NULL,
    arrives_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journey_passenger_groups (
    journey_id INTEGER NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
    passengers INTEGER NOT NULL,
    fare_cents INTEGER NOT NULL,
    PRIMARY KEY (journey_id, sequence)
);
CREATE TABLE IF NOT EXISTS journey_receipts (
    journey_id INTEGER PRIMARY KEY,
    revenue_cents INTEGER NOT NULL,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    train_id INTEGER,
    train_model_name TEXT,
    origin_station_id INTEGER,
    destination_station_id INTEGER,
    passengers_carried INTEGER,
    passenger_capacity INTEGER,
    completed_at INTEGER
);
"#;

fn query_all<T>(
    connection: &Connection,
    sql: &str,
    path: &Path,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, SaveSlotError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|source| db_error("prepare query for", path, source))?;
    let rows = statement
        .query_map([], |row| map(row))
        .map_err(|source| db_error("query", path, source))?;
    rows.map(|row| row.map_err(|source| db_error("decode row from", path, source)))
        .collect()
}

fn to_db_u64(value: u64, field: &'static str, path: &Path) -> Result<i64, SaveSlotError> {
    i64::try_from(value).map_err(|_| invalid_value(path, field))
}

fn row_domain_id<T>(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
    parse: impl FnOnce(&str) -> Result<T, uuid::Error>,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    parse(&value).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{field}: {source}"),
            )),
        )
    })
}

fn optional_row_domain_id<T>(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
    parse: impl Fn(&str) -> Result<T, uuid::Error>,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            parse(&value).map_err(|source| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{field}: {source}"),
                    )),
                )
            })
        })
        .transpose()
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize, field: &'static str) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| conversion_error(index, field))
}

fn optional_row_u32(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> rusqlite::Result<Option<u32>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| u32::try_from(value).map_err(|_| conversion_error(index, field)))
        .transpose()
}

fn conversion_error(index: usize, field: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Integer,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, field)),
    )
}

fn from_db_u64(value: i64, field: &'static str) -> Result<u64, &'static str> {
    u64::try_from(value).map_err(|_| field)
}

fn db_error(action: &'static str, path: &Path, source: rusqlite::Error) -> SaveSlotError {
    SaveSlotError::Database {
        action,
        path: path.to_path_buf(),
        source,
    }
}

fn invalid_value(path: &Path, field: &'static str) -> SaveSlotError {
    SaveSlotError::InvalidSave {
        path: path.to_path_buf(),
        source: Box::new(SaveCodecError::InvalidValue { field }),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::*;

    static NEXT_TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "railq-sqlite-storage-test-{}-{}",
                std::process::id(),
                NEXT_TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn save_path(&self) -> PathBuf {
            self.path.join("company.db")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn active_game() -> GameState {
        let departed_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        state.player_company.fleet.trains[0].nickname =
            Some(TrainNickname::parse("Morning Star").unwrap());
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
        state.origin_destination_demand[0].market_maturity =
            MarketMaturity::from_basis_points(4_321).unwrap();
        state.origin_destination_demand[0].fractional_passenger_seconds = 1_234;
        state
    }

    #[test]
    fn migrated_bidirectional_journey_snapshot_remains_loadable() {
        let directory = TestDirectory::new();
        let slot = SaveSlot::open(directory.save_path()).unwrap();
        let mut state = active_game();

        // Before schema v3, the same Passenger Service could be dispatched in
        // either direction. Simulate a migrated active Journey whose saved
        // direction is the reverse of the new directional Service order.
        state.player_company.passenger_services[0]
            .stop_station_ids
            .reverse();
        state.player_company.passenger_services[0]
            .rail_line_ids
            .reverse();
        state.active_journeys[0].current_stop_index = state.player_company.passenger_services[0]
            .stop_station_ids
            .len()
            .saturating_sub(1);

        slot.save(&state).unwrap();

        assert_eq!(slot.load().unwrap(), Some(state));
    }

    #[test]
    fn sqlite_round_trips_all_current_operating_state() {
        let directory = TestDirectory::new();
        let slot = SaveSlot::open(directory.save_path()).unwrap();
        let mut state = active_game();
        state.region.rail_authority.construction_capacity = 2;
        state.region.bulletin.push(BulletinEntry {
            occurred_at: UtcSeconds::from_unix_seconds(12_345),
            category: BulletinCategory::Authority,
            headline: "Alden connection approved".into(),
            detail: "The regional case passed formal review.".into(),
        });
        state.region.rail_authority.finances = RailAuthorityFinances {
            treasury: Money::from_cents(9_000_000),
            maintenance_reserve: Money::from_cents(1_500_000),
            committed_investment: Money::from_cents(2_000_000),
            carried_over_funds: Money::from_cents(750_000),
            regional_public_allocation: Money::from_cents(3_000_000),
            infrastructure_access_fee_revenue: Money::from_cents(425_000),
            next_fiscal_period_at: Some(UtcSeconds::from_unix_seconds(25_000)),
        };

        slot.save(&state).unwrap();

        assert_eq!(slot.load().unwrap(), Some(state));
    }

    #[test]
    fn sqlite_round_trips_all_infrastructure_project_kinds() {
        let directory = TestDirectory::new();
        let slot = SaveSlot::open(directory.save_path()).unwrap();
        let mut state = active_game();
        let timeline = |requested_at| InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(requested_at),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        let mut reconsidered_timeline = timeline(2_004);
        reconsidered_timeline.reconsideration_count = 2;
        state.region.rail_authority.infrastructure_projects = vec![
            InfrastructureProject {
                id: InfrastructureProjectId::new(1),
                kind: InfrastructureProjectKind::NewLine {
                    planned_stations: vec![PlannedRailStation {
                        id: RailStationId::new(5),
                        settlement_id: SettlementId::new(5),
                    }],
                    planned_lines: vec![PlannedRailLine {
                        id: RailLineId::new(4),
                        first_station_id: RailStationId::new(4),
                        second_station_id: RailStationId::new(5),
                        distance: DistanceMetres::new(12_000).unwrap(),
                        speed_limit: SpeedKilometresPerHour::new(70).unwrap(),
                        track_count: TrackCount::SINGLE,
                        electrification: Electrification::None,
                        construction_difficulty: ConstructionDifficulty::Moderate,
                    }],
                },
                status: InfrastructureProjectStatus::Requested,
                timeline: timeline(2_000),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(2),
                kind: InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: vec![RailLineId::new(1)],
                    target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
                },
                status: InfrastructureProjectStatus::UnderReview,
                timeline: timeline(2_001),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(3),
                kind: InfrastructureProjectKind::DoubleTracking {
                    rail_line_ids: vec![RailLineId::new(2)],
                    target_track_count: TrackCount::DOUBLE,
                },
                status: InfrastructureProjectStatus::Proposed,
                timeline: timeline(2_002),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(4),
                kind: InfrastructureProjectKind::Electrification {
                    rail_line_ids: vec![RailLineId::new(3)],
                },
                status: InfrastructureProjectStatus::Approved,
                timeline: timeline(2_003),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(5),
                kind: InfrastructureProjectKind::Renewal {
                    rail_line_ids: vec![RailLineId::new(1)],
                },
                status: InfrastructureProjectStatus::Deferred,
                timeline: reconsidered_timeline,
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(6),
                kind: InfrastructureProjectKind::StationUpgrade {
                    rail_station_ids: vec![RailStationId::new(1)],
                },
                status: InfrastructureProjectStatus::Funding,
                timeline: timeline(2_005),
                funding: InfrastructureProjectFunding::default(),
            },
        ];
        slot.save(&state).unwrap();

        assert_eq!(slot.load().unwrap(), Some(state));
    }

    #[test]
    fn validation_rejects_conflicting_projects_under_construction() {
        let mut state = active_game();
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(2_000),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        state.region.rail_authority.construction_capacity = 2;
        state.region.rail_authority.infrastructure_projects = vec![
            InfrastructureProject {
                id: InfrastructureProjectId::new(1),
                kind: InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: vec![RailLineId::new(1)],
                    target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
                },
                status: InfrastructureProjectStatus::Construction,
                timeline: timeline.clone(),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(2),
                kind: InfrastructureProjectKind::Electrification {
                    rail_line_ids: vec![RailLineId::new(1)],
                },
                status: InfrastructureProjectStatus::Construction,
                timeline,
                funding: InfrastructureProjectFunding::default(),
            },
        ];
        assert_eq!(
            validate_game_state(&state),
            Err(SaveValidationError::ImpossibleState {
                reason: "conflicting Infrastructure Projects are simultaneously under construction",
            })
        );
    }

    #[test]
    fn validation_allows_parallel_construction_on_disjoint_rail_lines() {
        let mut state = active_game();
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(2_000),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        state.region.rail_authority.construction_capacity = 2;
        state.region.rail_authority.infrastructure_projects = vec![
            InfrastructureProject {
                id: InfrastructureProjectId::new(1),
                kind: InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: vec![RailLineId::new(1)],
                    target_speed_limit: SpeedKilometresPerHour::new(100).unwrap(),
                },
                status: InfrastructureProjectStatus::Construction,
                timeline: timeline.clone(),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(2),
                kind: InfrastructureProjectKind::Renewal {
                    rail_line_ids: vec![RailLineId::new(2)],
                },
                status: InfrastructureProjectStatus::Construction,
                timeline,
                funding: InfrastructureProjectFunding::default(),
            },
        ];
        assert_eq!(validate_game_state(&state), Ok(()));
    }

    #[test]
    fn validation_allows_open_project_to_keep_historical_authority_commitment() {
        let mut state = active_game();
        let historical_commitment = Money::from_cents(250_000);
        state.region.rail_authority.finances.committed_investment = Money::ZERO;
        state.region.rail_authority.infrastructure_projects = vec![InfrastructureProject {
            id: InfrastructureProjectId::new_v4(),
            kind: InfrastructureProjectKind::Renewal {
                rail_line_ids: vec![state.region.rail_authority.rail_network.rail_lines[0].id],
            },
            status: InfrastructureProjectStatus::Open,
            timeline: InfrastructureProjectTimeline {
                requested_at: UtcSeconds::from_unix_seconds(1_000),
                review_started_at: Some(UtcSeconds::from_unix_seconds(1_100)),
                proposed_at: Some(UtcSeconds::from_unix_seconds(1_200)),
                approved_at: Some(UtcSeconds::from_unix_seconds(1_300)),
                funding_completed_at: Some(UtcSeconds::from_unix_seconds(1_400)),
                scheduled_start_at: Some(UtcSeconds::from_unix_seconds(1_500)),
                construction_started_at: Some(UtcSeconds::from_unix_seconds(1_600)),
                planned_completion_at: Some(UtcSeconds::from_unix_seconds(1_700)),
                completed_at: Some(UtcSeconds::from_unix_seconds(1_700)),
                deferred_at: None,
                cancelled_at: None,
                reconsideration_count: 0,
            },
            funding: InfrastructureProjectFunding {
                estimated_cost: historical_commitment,
                authority_committed: historical_commitment,
                operator_contributed: Money::ZERO,
                access_fee_credit_awarded: Money::ZERO,
                access_fee_credit_remaining: Money::ZERO,
            },
        }];

        assert_eq!(validate_game_state(&state), Ok(()));
    }

    #[test]
    fn validation_rejects_more_active_projects_than_construction_capacity() {
        let mut state = active_game();
        state.region.rail_authority.construction_capacity = 1;
        let timeline = InfrastructureProjectTimeline {
            requested_at: UtcSeconds::from_unix_seconds(2_000),
            review_started_at: None,
            proposed_at: None,
            approved_at: None,
            funding_completed_at: None,
            scheduled_start_at: None,
            construction_started_at: None,
            planned_completion_at: None,
            completed_at: None,
            deferred_at: None,
            cancelled_at: None,
            reconsideration_count: 0,
        };
        state.region.rail_authority.infrastructure_projects = vec![
            InfrastructureProject {
                id: InfrastructureProjectId::new(101),
                kind: InfrastructureProjectKind::Renewal {
                    rail_line_ids: vec![RailLineId::new(1)],
                },
                status: InfrastructureProjectStatus::Construction,
                timeline: timeline.clone(),
                funding: InfrastructureProjectFunding::default(),
            },
            InfrastructureProject {
                id: InfrastructureProjectId::new(102),
                kind: InfrastructureProjectKind::Renewal {
                    rail_line_ids: vec![RailLineId::new(2)],
                },
                status: InfrastructureProjectStatus::Construction,
                timeline,
                funding: InfrastructureProjectFunding::default(),
            },
        ];

        assert_eq!(
            validate_game_state(&state),
            Err(SaveValidationError::ImpossibleState {
                reason: "Rail Authority active construction exceeds its capacity",
            })
        );
    }

    #[test]
    fn save_uses_normalized_tables_instead_of_one_state_blob() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();
        slot.save(&active_game()).unwrap();

        let connection = Connection::open(path).unwrap();
        let train_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM trains", [], |row| row.get(0))
            .unwrap();
        let journey_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM active_journeys", [], |row| row.get(0))
            .unwrap();
        let service_stop_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM service_stops", [], |row| row.get(0))
            .unwrap();
        let passenger_group_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM journey_passenger_groups", [], |row| {
                row.get(0)
            })
            .unwrap();
        let state_blob_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='game_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let saved_catalogue_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='diesel_catalogue'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let train_id: String = connection
            .query_row("SELECT id FROM trains LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let station_id: String = connection
            .query_row("SELECT id FROM rail_stations LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let model_id: String = connection
            .query_row("SELECT model_id FROM trains LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let evn: String = connection
            .query_row("SELECT evn FROM trains LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let next_train_display_number: i64 = connection
            .query_row(
                "SELECT next_train_display_number FROM company WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let next_unit_number: i64 = connection
            .query_row(
                "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(train_count, 1);
        assert_eq!(journey_count, 1);
        assert_eq!(service_stop_count, 2);
        assert!(passenger_group_count > 0);
        assert_eq!(state_blob_table, 0);
        assert_eq!(saved_catalogue_table, 0);
        for id in [&train_id, &station_id] {
            uuid::Uuid::parse_str(id).unwrap();
            assert_eq!(&id[14..15], "4");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        }
        assert_eq!(model_id, "helvetra-r70");
        let evn = EuropeanVehicleNumber::parse(&evn).unwrap();
        assert_eq!(evn.registration_code(), 67);
        assert_eq!(evn.series_code(), 701);
        assert_eq!(evn.unit_number(), 1);
        assert_eq!(next_train_display_number, 2);
        assert_eq!(next_unit_number, 2);
    }

    #[test]
    fn v9_catalogue_ids_migrate_to_replacement_models() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        {
            let slot = SaveSlot::open(&path).unwrap();
            let mut state =
                create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
            state.player_company.funds = Money::from_cents(1_000_000);
            purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
            purchase_train(&mut state, 1, RailStationId::new(1)).unwrap();
            slot.save(&state).unwrap();
        }

        let connection = Connection::open(&path).unwrap();
        let old_local_evn = EuropeanVehicleNumber::generate(95, 67, 70, 1).unwrap();
        let old_express_evn = EuropeanVehicleNumber::generate(95, 67, 120, 1).unwrap();
        connection
            .execute(
                "UPDATE trains SET model_id = 'local-70', evn = ?1 WHERE model_id = 'helvetra-r70'",
                params![old_local_evn.as_str()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE trains SET model_id = 'express-120', evn = ?1 WHERE model_id = 'veltrian-d121'",
                params![old_express_evn.as_str()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE train_model_sequences SET model_id = 'local-70' WHERE model_id = 'helvetra-r70'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE train_model_sequences SET model_id = 'express-120' WHERE model_id = 'veltrian-d121'",
                [],
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", 9_u32)
            .unwrap();

        // Exercise only the catalogue-ID migration here. The database was
        // created with the current schema, so running the entire historical
        // schema chain would intentionally try to re-add later columns.
        migrate_v9_to_v10(&connection, &path).unwrap();

        let migrated_models = query_all(
            &connection,
            "SELECT model_id, evn FROM trains ORDER BY evn",
            &path,
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
        assert_eq!(
            migrated_models
                .iter()
                .map(|(model_id, evn)| {
                    (
                        model_id.as_str(),
                        EuropeanVehicleNumber::parse(evn).unwrap().series_code(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![("helvetra-r70", 701), ("veltrian-d121", 721)]
        );
        let local_next: i64 = connection
            .query_row(
                "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let express_next: i64 = connection
            .query_row(
                "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'veltrian-d121'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(local_next, 2);
        assert_eq!(express_next, 2);
    }

    #[test]
    fn missing_database_is_reported_without_creating_a_fresh_game() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();

        assert_eq!(slot.load().unwrap(), None);
        assert!(!path.exists());
    }

    #[test]
    fn second_opener_cannot_own_the_same_save_slot() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let _first_slot = SaveSlot::open(&path).unwrap();

        assert!(matches!(
            SaveSlot::open(path),
            Err(SaveSlotError::AlreadyOwned { .. })
        ));
    }

    #[test]
    fn unsupported_schema_version_is_preserved_and_rejected() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SAVE_VERSION + 1)
            .unwrap();
        drop(connection);
        let slot = SaveSlot::open(&path).unwrap();

        assert!(matches!(
            slot.load(),
            Err(SaveSlotError::InvalidSave { .. })
        ));
        assert!(path.exists());
    }

    #[test]
    fn restart_backup_preserves_the_old_database_before_replacing_it() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();
        let old_state = active_game();
        slot.save(&old_state).unwrap();
        let replacement = create_new_game(99, "New Passenger", UtcSeconds::from_unix_seconds(2));

        let backup_path = slot.save_after_backup(&replacement).unwrap();

        assert!(backup_path.exists());
        let backup_slot = SaveSlot::open(&backup_path).unwrap();
        assert_eq!(backup_slot.load().unwrap(), Some(old_state));
        assert_eq!(slot.load().unwrap(), Some(replacement));
    }

    #[test]
    fn v1_sqlite_schema_migrates_owned_trains_to_stable_model_ids() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE game_meta (
                     singleton INTEGER PRIMARY KEY,
                     world_seed TEXT NOT NULL,
                     last_processed_at INTEGER NOT NULL
                 );
                 CREATE TABLE region (
                     singleton INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     population INTEGER NOT NULL,
                     rail_authority_name TEXT NOT NULL
                 );
                 INSERT INTO game_meta VALUES(1, '42', 0);
                 INSERT INTO region VALUES(1, 'Federation of Varelia', 1000, 'Federation of Varelia Rail Authority');
                 CREATE TABLE company (
                     singleton INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     funds_cents INTEGER NOT NULL
                 );
                 INSERT INTO company VALUES(1, 'One More Prime', 1000000);
                 CREATE TABLE trains (
                     id INTEGER PRIMARY KEY,
                     status_kind TEXT NOT NULL,
                     status_ref_id TEXT NOT NULL,
                     model_name TEXT NOT NULL,
                     original_purchase_price_cents INTEGER NOT NULL,
                     passenger_capacity INTEGER NOT NULL,
                     speed_metres_per_second INTEGER NOT NULL,
                     fuel_cost_cents_per_km INTEGER NOT NULL
                 );
                 CREATE TABLE diesel_catalogue (
                     sequence INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     purchase_price_cents INTEGER NOT NULL,
                     passenger_capacity INTEGER NOT NULL,
                     speed_metres_per_second INTEGER NOT NULL,
                     fuel_cost_cents_per_km INTEGER NOT NULL
                 );
                 CREATE TABLE rail_stations (id INTEGER PRIMARY KEY);
                 CREATE TABLE rail_lines (id INTEGER PRIMARY KEY);
                 CREATE TABLE passenger_services (
                     id INTEGER PRIMARY KEY,
                     first_station_id INTEGER NOT NULL,
                     second_station_id INTEGER NOT NULL
                 );
                 CREATE TABLE service_lines (
                     service_id INTEGER NOT NULL,
                     sequence INTEGER NOT NULL,
                     rail_line_id INTEGER NOT NULL,
                     PRIMARY KEY (service_id, sequence)
                 );
                 CREATE TABLE active_journeys (
                     id INTEGER PRIMARY KEY,
                     service_id INTEGER NOT NULL,
                     train_id INTEGER NOT NULL REFERENCES trains(id),
                     origin_station_id INTEGER NOT NULL,
                     destination_station_id INTEGER NOT NULL,
                     passengers_carried INTEGER NOT NULL,
                     fare_cents INTEGER NOT NULL,
                     operating_revenue_cents INTEGER NOT NULL,
                     infrastructure_access_fee_cents INTEGER NOT NULL,
                     fuel_cost_cents INTEGER NOT NULL,
                     departed_at INTEGER NOT NULL,
                     arrives_at INTEGER NOT NULL
                 );
                 INSERT INTO rail_stations VALUES(1);
                 INSERT INTO rail_stations VALUES(2);
                 INSERT INTO rail_lines VALUES(1);
                 INSERT INTO passenger_services VALUES(1, 1, 2);
                 INSERT INTO service_lines VALUES(1, 0, 1);
                 INSERT INTO trains VALUES(1, 'travelling', 1, 'Local 70', 300000, 70, 25, 45);
                 INSERT INTO diesel_catalogue VALUES(0, 'Local 70', 300000, 70, 25, 45);
                 INSERT INTO active_journeys VALUES(1, 1, 1, 1, 2, 1, 100, 100, 10, 10, 0, 60);
                 PRAGMA user_version = 1;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let migrated_train_id = "00000005-0000-4000-8000-000000000001";
        let migrated_journey_id = "00000007-0000-4000-8000-000000000001";
        let migrated_service_id = "00000006-0000-4000-8000-000000000001";
        let model_id: String = connection
            .query_row(
                "SELECT model_id FROM trains WHERE id = ?1",
                params![migrated_train_id],
                |row| row.get(0),
            )
            .unwrap();
        let evn: String = connection
            .query_row(
                "SELECT evn FROM trains WHERE id = ?1",
                params![migrated_train_id],
                |row| row.get(0),
            )
            .unwrap();
        let next_train_display_number: i64 = connection
            .query_row(
                "SELECT next_train_display_number FROM company WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let next_unit_number: i64 = connection
            .query_row(
                "SELECT next_unit_number FROM train_model_sequences WHERE model_id = 'helvetra-r70'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let old_catalogue_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='diesel_catalogue'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let journey_train_id: String = connection
            .query_row(
                "SELECT train_id FROM active_journeys WHERE id = ?1",
                params![migrated_journey_id],
                |row| row.get(0),
            )
            .unwrap();
        let service_name: String = connection
            .query_row(
                "SELECT name FROM passenger_services WHERE id = ?1",
                params![migrated_service_id],
                |row| row.get(0),
            )
            .unwrap();
        let service_stop_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM service_stops WHERE service_id = ?1",
                params![migrated_service_id],
                |row| row.get(0),
            )
            .unwrap();
        let foreign_key_violations: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        let (registration_code, registration_mark): (i64, String) = connection
            .query_row(
                "SELECT registration_code, registration_mark FROM region WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let company_vkm: String = connection
            .query_row("SELECT vkm FROM company WHERE singleton = 1", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(model_id, "helvetra-r70");
        assert_eq!(evn, "956707010016");
        assert_eq!(next_train_display_number, 2);
        assert_eq!(next_unit_number, 2);
        assert_eq!(old_catalogue_exists, 0);
        assert_eq!(journey_train_id, migrated_train_id);
        assert_eq!(service_name, "R1");
        assert_eq!(service_stop_count, 2);
        assert_eq!(foreign_key_violations, 0);
        assert_eq!(registration_code, 67);
        assert_eq!(registration_mark, "VA");
        assert_eq!(company_vkm, "OMP");
    }

    #[test]
    fn v12_schema_migrates_rail_authority_finances_with_safe_defaults() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE region (
                     singleton INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     registration_code INTEGER NOT NULL,
                     registration_mark TEXT NOT NULL,
                     population INTEGER NOT NULL,
                     rail_authority_name TEXT NOT NULL,
                     next_infrastructure_project_id INTEGER NOT NULL
                 );
                 INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority', 1);
                 PRAGMA user_version = 12;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let finances: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT treasury_cents, maintenance_reserve_cents, committed_investment_cents,
                        carried_over_funds_cents
                 FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(
            finances,
            (
                crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents(),
                0,
                0,
                0
            )
        );
    }

    #[test]
    fn v17_schema_adds_rail_authority_construction_capacity() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE region (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     name TEXT NOT NULL,
                     registration_code INTEGER NOT NULL,
                     registration_mark TEXT NOT NULL,
                     population INTEGER NOT NULL,
                     rail_authority_name TEXT NOT NULL
                 );
                 INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
                 PRAGMA user_version = 17;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let capacity: i64 = connection
            .query_row(
                "SELECT rail_authority_construction_capacity FROM region WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(
            capacity,
            i64::from(crate::model::PROVISIONAL_CONSTRUCTION_CAPACITY)
        );
    }

    #[test]
    fn v16_schema_migrates_entity_ids_to_uuid_v4_text() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(V16_IDENTITY_TABLES_COMPAT_SCHEMA)
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 2000, 'Varelia Rail Authority', 2);
                 INSERT INTO settlements VALUES(1, 'Alden', 1000);
                 INSERT INTO settlements VALUES(2, 'Bellhaven', 1000);
                 INSERT INTO rail_stations VALUES(1, 1);
                 INSERT INTO rail_stations VALUES(2, 2);
                 INSERT INTO rail_lines VALUES(1, 1, 2, 10000, 70, 1, 'none', 'moderate');
                 INSERT INTO infrastructure_projects(
                     id, kind, status, requested_at, target_speed_limit_kmh
                 ) VALUES(1, 'speed_upgrade', 'requested', 1000, 100);
                 INSERT INTO infrastructure_project_rail_lines VALUES(1, 0, 1);
                 INSERT INTO company VALUES(1, 'One More Prime', 'OMP', 500000, 2);
                 INSERT INTO trains VALUES(1, '956707010016', NULL, 'travelling', 1, 'helvetra-r70', 300000);
                 INSERT INTO passenger_services VALUES(1, 'R1');
                 INSERT INTO service_stops VALUES(1, 0, 1);
                 INSERT INTO service_stops VALUES(1, 1, 2);
                 INSERT INTO service_lines VALUES(1, 0, 1);
                 INSERT INTO origin_destination_demand VALUES(1, 2, 10, 20, 0);
                 INSERT INTO active_journeys(
                     id, service_id, train_id, origin_station_id, destination_station_id,
                     passengers_carried, fare_cents, operating_revenue_cents,
                     credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents,
                     current_stop_index, departed_at, arrives_at
                 ) VALUES(1, 1, 1, 1, 2, 10, 1000, 1000, 0, 100, 50, 1, 1000, 1100);
                 INSERT INTO journey_passenger_groups VALUES(1, 0, 1, 2, 10, 1000);
                 INSERT INTO journey_receipts VALUES(1, 1000, 100, 50, 1, 'Helvetra R70', 1, 2, 10, 70, 1100);
                 PRAGMA user_version = 16;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let train_id: String = connection
            .query_row("SELECT id FROM trains", [], |row| row.get(0))
            .unwrap();
        let model_id: String = connection
            .query_row("SELECT model_id FROM trains", [], |row| row.get(0))
            .unwrap();
        let line_id: String = connection
            .query_row("SELECT id FROM rail_lines", [], |row| row.get(0))
            .unwrap();
        let project_id: String = connection
            .query_row("SELECT id FROM infrastructure_projects", [], |row| {
                row.get(0)
            })
            .unwrap();
        let journey_train_id: String = connection
            .query_row("SELECT train_id FROM active_journeys", [], |row| row.get(0))
            .unwrap();
        let foreign_key_violations: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();

        for id in [&train_id, &line_id, &project_id] {
            uuid::Uuid::parse_str(id).unwrap();
            assert_eq!(&id[14..15], "4");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        }
        assert_eq!(version, SAVE_VERSION);
        assert_eq!(train_id, "00000005-0000-4000-8000-000000000001");
        assert_eq!(line_id, "00000003-0000-4000-8000-000000000001");
        assert_eq!(project_id, "00000004-0000-4000-8000-000000000001");
        assert_eq!(journey_train_id, train_id);
        assert_eq!(model_id, "helvetra-r70");
        assert_eq!(foreign_key_violations, 0);
    }

    #[test]
    fn v22_schema_aligns_the_next_authority_fiscal_period_to_utc_midnight() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                     regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
                     infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000, 0);
                 CREATE TABLE game_meta (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     world_seed TEXT NOT NULL,
                     last_processed_at INTEGER NOT NULL
                 );
                 INSERT INTO game_meta VALUES(1, '42', 1000);
                 PRAGMA user_version = 22;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let next_fiscal_period_at: i64 = connection
            .query_row(
                "SELECT next_fiscal_period_at FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(next_fiscal_period_at, 86_400);
    }

    #[test]
    fn v23_schema_realigns_existing_fiscal_schedule_to_next_utc_midnight() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                     regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
                     infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0,
                     next_fiscal_period_at INTEGER
                 );
                 INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000, 0, 22600);
                 CREATE TABLE game_meta (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     world_seed TEXT NOT NULL,
                     last_processed_at INTEGER NOT NULL
                 );
                 INSERT INTO game_meta VALUES(1, '42', 50000);
                 PRAGMA user_version = 23;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let next_fiscal_period_at: i64 = connection
            .query_row(
                "SELECT next_fiscal_period_at FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(next_fiscal_period_at, 86_400);
    }

    #[test]
    fn v25_schema_backfills_project_reconsideration_count() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE infrastructure_projects (
                     id TEXT PRIMARY KEY
                 );
                 INSERT INTO infrastructure_projects(id) VALUES('project-1');
                 PRAGMA user_version = 25;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let reconsideration_count: i64 = connection
            .query_row(
                "SELECT reconsideration_count FROM infrastructure_projects WHERE id = 'project-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(reconsideration_count, 0);
    }

    #[test]
    fn v26_schema_adds_persistent_railway_bulletin() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 26;")
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let bulletin_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'bulletin_entries'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(bulletin_table, 1);
    }

    #[test]
    fn v24_schema_backfills_existing_markets_as_fully_mature() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE origin_destination_demand (
                     origin_station_id TEXT NOT NULL,
                     destination_station_id TEXT NOT NULL,
                     sequence INTEGER NOT NULL UNIQUE,
                     waiting_passengers INTEGER NOT NULL,
                     passenger_arrival_rate_per_hour INTEGER NOT NULL,
                     fractional_passenger_seconds INTEGER NOT NULL,
                     PRIMARY KEY (origin_station_id, destination_station_id)
                 );
                 INSERT INTO origin_destination_demand VALUES('a', 'b', 0, 12, 4, 0);
                 PRAGMA user_version = 24;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let maturity: i64 = connection
            .query_row(
                "SELECT market_maturity_basis_points FROM origin_destination_demand",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(maturity, 10_000);
    }

    #[test]
    fn v15_schema_credits_historical_access_fees_to_authority() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                     regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0)
                 );
                 INSERT INTO rail_authority_finances VALUES(1, 10000000, 500000, 0, 0, 10000000);
                 CREATE TABLE financials (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     operating_revenue_cents INTEGER NOT NULL,
                     infrastructure_access_fees_cents INTEGER NOT NULL,
                     fuel_costs_cents INTEGER NOT NULL
                 );
                 INSERT INTO financials VALUES(1, 2000000, 325000, 175000);
                 PRAGMA user_version = 15;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let (treasury, access_fee_revenue): (i64, i64) = connection
            .query_row(
                "SELECT treasury_cents, infrastructure_access_fee_revenue_cents
                 FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(access_fee_revenue, 325_000);
        assert_eq!(treasury, 10_325_000);
    }

    #[test]
    fn v14_schema_seeds_network_sized_maintenance_reserve() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
                     regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0)
                 );
                 INSERT INTO rail_authority_finances VALUES(1, 10000000, 0, 1000000, 0, 10000000);
                 CREATE TABLE settlements (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     population INTEGER NOT NULL
                 );
                 INSERT INTO settlements VALUES(1, 'A', 1000);
                 INSERT INTO settlements VALUES(2, 'B', 1000);
                 INSERT INTO settlements VALUES(3, 'C', 1000);
                 CREATE TABLE rail_stations (
                     id INTEGER PRIMARY KEY,
                     settlement_id INTEGER NOT NULL REFERENCES settlements(id)
                 );
                 INSERT INTO rail_stations VALUES(1, 1);
                 INSERT INTO rail_stations VALUES(2, 2);
                 INSERT INTO rail_stations VALUES(3, 3);
                 CREATE TABLE rail_lines (
                     id INTEGER PRIMARY KEY,
                     first_station_id INTEGER NOT NULL,
                     second_station_id INTEGER NOT NULL,
                     distance_metres INTEGER NOT NULL,
                     speed_limit_kmh INTEGER NOT NULL,
                     track_count INTEGER NOT NULL,
                     electrification TEXT NOT NULL,
                     construction_difficulty TEXT NOT NULL
                 );
                 INSERT INTO rail_lines VALUES(1, 1, 2, 10000, 70, 1, 'none', 'moderate');
                 INSERT INTO rail_lines VALUES(2, 2, 3, 5000, 70, 2, 'none', 'moderate');
                 PRAGMA user_version = 14;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let reserve: i64 = connection
            .query_row(
                "SELECT maintenance_reserve_cents
                 FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(reserve, 500_000);
    }

    #[test]
    fn v13_schema_migrates_and_seeds_regional_public_allocation() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0)
                 );
                 INSERT INTO rail_authority_finances VALUES(1, 5000, 1000, 2000, 500);
                 PRAGMA user_version = 13;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let (treasury, allocation): (i64, i64) = connection
            .query_row(
                "SELECT treasury_cents, regional_public_allocation_cents
                 FROM rail_authority_finances WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(
            allocation,
            crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents()
        );
        assert_eq!(treasury, 5000 + allocation);
    }

    #[test]
    fn v11_schema_migrates_infrastructure_project_persistence() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE region (
                     singleton INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     registration_code INTEGER NOT NULL,
                     registration_mark TEXT NOT NULL,
                     population INTEGER NOT NULL,
                     rail_authority_name TEXT NOT NULL
                 );
                 INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
                 CREATE TABLE settlements (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     population INTEGER NOT NULL
                 );
                 CREATE TABLE rail_stations (
                     id INTEGER PRIMARY KEY,
                     settlement_id INTEGER NOT NULL REFERENCES settlements(id)
                 );
                 CREATE TABLE rail_lines (
                     id INTEGER PRIMARY KEY,
                     first_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                     second_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                     distance_metres INTEGER NOT NULL,
                     speed_limit_kmh INTEGER NOT NULL,
                     track_count INTEGER NOT NULL,
                     electrification TEXT NOT NULL,
                     construction_difficulty TEXT NOT NULL
                 );
                 PRAGMA user_version = 11;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let project_table_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'infrastructure_projects'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(project_table_exists, 1);
    }

    #[test]
    fn v10_schema_migrates_rail_line_capabilities_with_safe_defaults() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE region (
                     singleton INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     registration_code INTEGER NOT NULL,
                     registration_mark TEXT NOT NULL,
                     population INTEGER NOT NULL,
                     rail_authority_name TEXT NOT NULL
                 );
                 INSERT INTO region VALUES(1, 'Federation of Varelia', 67, 'VA', 1000, 'Varelia Rail Authority');
                 CREATE TABLE settlements (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     population INTEGER NOT NULL
                 );
                 INSERT INTO settlements VALUES(1, 'A', 500);
                 INSERT INTO settlements VALUES(2, 'B', 500);
                 CREATE TABLE rail_stations (
                     id INTEGER PRIMARY KEY,
                     settlement_id INTEGER NOT NULL REFERENCES settlements(id)
                 );
                 INSERT INTO rail_stations VALUES(1, 1);
                 INSERT INTO rail_stations VALUES(2, 2);
                 CREATE TABLE rail_lines (
                     id INTEGER PRIMARY KEY,
                     first_station_id INTEGER NOT NULL,
                     second_station_id INTEGER NOT NULL,
                     distance_metres INTEGER NOT NULL
                 );
                 INSERT INTO rail_lines VALUES(1, 1, 2, 42000);
                 PRAGMA user_version = 10;",
            )
            .unwrap();

        ensure_schema(&connection, &path).unwrap();

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let capabilities: (i64, i64, String, String) = connection
            .query_row(
                "SELECT speed_limit_kmh, track_count, electrification, construction_difficulty
                 FROM rail_lines WHERE id = '00000003-0000-4000-8000-000000000001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(capabilities, (70, 1, "none".into(), "moderate".into()));
    }

    #[test]
    fn legacy_ron_decoder_accepts_the_previous_versioned_envelope() {
        let state = active_game();
        let source = legacy::encode_v1_for_test(&state);

        assert_eq!(decode_legacy_game_state(&source).unwrap(), state);
    }
}

//! SQLite persistence for RailQ.
//!
//! Mutable game state is stored in normalized tables rather than one growing
//! RON document. The application still works with a validated [`GameState`]
//! at its boundary; this module is responsible for reconstructing that state
//! transactionally and checking every simulation invariant before publishing
//! it to the rest of the program.

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
use serde::{Deserialize, Serialize};

use crate::{
    balance::BalanceConfig,
    catalog::{model_for_train, train_catalogue},
    model::{
        CalculationError, DemandRules, DistanceMetres, DurationSeconds, Financials, Fleet,
        GameRules, GameState, Journey, JourneyId, JourneyPassengerGroup, JourneyReceipt, Money, MoneyPerKilometre,
        OriginDestinationDemand, PassengerArrivalRate, PassengerCapacity, PassengerService,
        PlayerCompany, RailAuthority, RailLine, RailLineId, RailNetwork, RailStation,
        RailStationId, Region, RailwayRegistration, ServiceId, Settlement, SettlementId,
        SpeedMetresPerSecond, Train, TrainId, TrainModelId, TrainStatus, UtcSeconds,
        VehicleKeeperMark,
    },
    sim::{
        services::{path_between_stations, service_path_for_stops},
        world::railway_registration_for_existing_region,
    },
};

/// SQLite schema understood by this build.
pub const SAVE_VERSION: u32 = 6;

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
    InvalidPath { path: PathBuf },
    AlreadyOwned { path: PathBuf },
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
            Self::Io { action, path, source } => {
                write!(formatter, "could not {action} {}: {source}", path.display())
            }
            Self::Database { action, path, source } => {
                write!(formatter, "could not {action} SQLite save {}: {source}", path.display())
            }
            Self::InvalidSave { path, source } => {
                write!(formatter, "save {} is invalid and was preserved: {source}", path.display())
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
            Self::LegacyDecode(error) => write!(formatter, "could not decode legacy RON save: {error}"),
        }
    }
}

impl Error for SaveCodecError {}

impl SaveSlot {
    pub fn open_default() -> Result<Self, SaveSlotError> {
        let database_existed = Path::new(DEFAULT_SAVE_PATH).exists();
        let slot = Self::open(DEFAULT_SAVE_PATH)?;
        if !database_existed && Path::new(LEGACY_SAVE_PATH).exists() {
            let source = fs::read_to_string(LEGACY_SAVE_PATH).map_err(|source| SaveSlotError::Io {
                action: "read legacy RON save",
                path: PathBuf::from(LEGACY_SAVE_PATH),
                source,
            })?;
            let state = decode_legacy_game_state(&source).map_err(|source| SaveSlotError::InvalidSave {
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
            Ok(()) => Ok(Self { path, _lock_file: lock_file }),
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
        let transaction = connection.transaction().map_err(|source| SaveSlotError::Database {
            action: "begin transaction for",
            path: self.path.clone(),
            source,
        })?;
        clear_state(&transaction, &self.path, preserve_history)?;
        insert_state(&transaction, state, &self.path)?;
        transaction.commit().map_err(|source| SaveSlotError::Database {
            action: "commit transaction for",
            path: self.path.clone(),
            source,
        })?;
        connection.execute_batch("PRAGMA optimize;").map_err(|source| SaveSlotError::Database {
            action: "optimize",
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    fn open_connection(&self, action: &'static str) -> Result<Connection, SaveSlotError> {
        let connection = Connection::open(&self.path).map_err(|source| SaveSlotError::Database {
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
        return Err(SaveSlotError::InvalidPath { path: path.to_path_buf() });
    };
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

fn archive_save(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "save path has no file name")
    })?;
    for _ in 0..128 {
        let sequence = NEXT_ARCHIVE_ID.fetch_add(1, Ordering::Relaxed);
        let archive_path = parent.join(format!(
            "{}.bankrupt-backup-{}.{}.db",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&archive_path) {
            Ok(_) => {
                fs::copy(path, &archive_path)?;
                return Ok(archive_path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a unique restart backup save"))
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS game_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    world_seed TEXT NOT NULL,
    last_processed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS region (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    registration_code INTEGER NOT NULL CHECK (registration_code BETWEEN 10 AND 99),
    registration_mark TEXT NOT NULL,
    population INTEGER NOT NULL,
    rail_authority_name TEXT NOT NULL
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
    distance_metres INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS company (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    vkm TEXT NOT NULL CHECK (length(vkm) BETWEEN 2 AND 5) CHECK (vkm NOT GLOB '*[^A-Z]*'),
    funds_cents INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS trains (
    id INTEGER PRIMARY KEY,
    status_kind TEXT NOT NULL CHECK (status_kind IN ('ready', 'travelling')),
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
    credited_revenue_cents INTEGER NOT NULL,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    current_stop_index INTEGER NOT NULL,
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
CREATE TABLE IF NOT EXISTS financials (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    operating_revenue_cents INTEGER NOT NULL,
    infrastructure_access_fees_cents INTEGER NOT NULL,
    fuel_costs_cents INTEGER NOT NULL
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
CREATE TABLE IF NOT EXISTS game_rules (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    fare_cents_per_passenger_km INTEGER NOT NULL,
    access_fee_cents_per_train_km INTEGER NOT NULL,
    starting_company_funds_cents INTEGER NOT NULL,
    demand_cap_seconds INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);
CREATE INDEX IF NOT EXISTS idx_receipts_completed_at ON journey_receipts(completed_at);
"#;

fn ensure_schema(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| SaveSlotError::Database {
            action: "read schema version from",
            path: path.to_path_buf(),
            source,
        })?;

    match version {
        0 => {
            connection.execute_batch(SCHEMA).map_err(|source| SaveSlotError::Database {
                action: "initialize schema for",
                path: path.to_path_buf(),
                source,
            })?;
            connection
                .pragma_update(None, "user_version", SAVE_VERSION)
                .map_err(|source| SaveSlotError::Database {
                    action: "write schema version to",
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        1 => {
            migrate_v1_to_v2(connection, path)?;
            migrate_v2_to_v3(connection, path)?;
            migrate_v3_to_v4(connection, path)?;
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
        }
        2 => {
            migrate_v2_to_v3(connection, path)?;
            migrate_v3_to_v4(connection, path)?;
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
        }
        3 => {
            migrate_v3_to_v4(connection, path)?;
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
        }
        4 => {
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
        }
        5 => migrate_v5_to_v6(connection, path)?,
        SAVE_VERSION => {
            connection.execute_batch(SCHEMA).map_err(|source| SaveSlotError::Database {
                action: "verify schema for",
                path: path.to_path_buf(),
                source,
            })?;
        }
        found => {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::UnsupportedVersion { found }),
            });
        }
    }
    Ok(())
}

fn migrate_v1_to_v2(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE trains RENAME TO trains_v1;
             CREATE TABLE trains (
                 id INTEGER PRIMARY KEY,
                 status_kind TEXT NOT NULL CHECK (status_kind IN ('ready', 'travelling')),
                 status_ref_id INTEGER NOT NULL,
                 model_id TEXT NOT NULL,
                 original_purchase_price_cents INTEGER NOT NULL
             );",
        )
        .map_err(|source| db_error("begin v1 to v2 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let mut statement = connection
            .prepare(
                "SELECT id, status_kind, status_ref_id, model_name, original_purchase_price_cents,
                        passenger_capacity, speed_metres_per_second, fuel_cost_cents_per_km
                 FROM trains_v1 ORDER BY id",
            )
            .map_err(|source| db_error("read v1 Trains from", path, source))?;
        let mut rows = statement
            .query([])
            .map_err(|source| db_error("read v1 Trains from", path, source))?;
        while let Some(row) = rows
            .next()
            .map_err(|source| db_error("read v1 Train row from", path, source))?
        {
            let model_name: String = row
                .get(3)
                .map_err(|source| db_error("decode v1 Train model from", path, source))?;
            let passenger_capacity: i64 = row
                .get(5)
                .map_err(|source| db_error("decode v1 Train capacity from", path, source))?;
            let speed: i64 = row
                .get(6)
                .map_err(|source| db_error("decode v1 Train speed from", path, source))?;
            let fuel_rate: i64 = row
                .get(7)
                .map_err(|source| db_error("decode v1 Train fuel rate from", path, source))?;
            let model = train_catalogue()
                .models()
                .iter()
                .find(|model| {
                    model.name() == model_name
                        && i64::from(model.passenger_capacity().passengers()) == passenger_capacity
                        && i64::try_from(model.speed().metres_per_second()).ok() == Some(speed)
                        && i64::try_from(model.fuel_cost_per_kilometre().cents_per_kilometre())
                            .ok()
                            == Some(fuel_rate)
                })
                .ok_or_else(|| SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::TrainModelNotFound {
                        model_name: model_name.clone(),
                    }),
                })?;

            connection
                .execute(
                    "INSERT INTO trains(id, status_kind, status_ref_id, model_id, original_purchase_price_cents)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        row.get::<_, i64>(0).map_err(|source| db_error("decode v1 Train ID from", path, source))?,
                        row.get::<_, String>(1).map_err(|source| db_error("decode v1 Train status from", path, source))?,
                        row.get::<_, i64>(2).map_err(|source| db_error("decode v1 Train status reference from", path, source))?,
                        model.id().as_str(),
                        row.get::<_, i64>(4).map_err(|source| db_error("decode v1 Train price from", path, source))?,
                    ],
                )
                .map_err(|source| db_error("write migrated Train to", path, source))?;
        }
        drop(rows);
        drop(statement);
        connection
            .execute_batch(
                "DROP TABLE trains_v1;
                 DROP TABLE IF EXISTS diesel_catalogue;",
            )
            .map_err(|source| db_error("finish v1 to v2 schema migration for", path, source))?;
        connection
            .pragma_update(None, "user_version", 2_u32)
            .map_err(|source| db_error("write v2 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row("SELECT 1 FROM pragma_foreign_key_check LIMIT 1", [], |row| row.get(0))
            .optional()
            .map_err(|source| db_error("verify v1 to v2 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v1 to v2 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA legacy_alter_table = OFF;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v1 to v2 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA legacy_alter_table = OFF;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v2_to_v3(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE active_journeys RENAME TO active_journeys_v2;
             ALTER TABLE service_lines RENAME TO service_lines_v2;
             ALTER TABLE passenger_services RENAME TO passenger_services_v2;
             CREATE TABLE passenger_services (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE service_stops (
                 service_id INTEGER NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE service_lines (
                 service_id INTEGER NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 rail_line_id INTEGER NOT NULL REFERENCES rail_lines(id),
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE active_journeys (
                 id INTEGER PRIMARY KEY,
                 service_id INTEGER NOT NULL REFERENCES passenger_services(id),
                 train_id INTEGER NOT NULL REFERENCES trains(id),
                 origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 passengers_carried INTEGER NOT NULL,
                 fare_cents INTEGER NOT NULL,
                 operating_revenue_cents INTEGER NOT NULL,
                 infrastructure_access_fee_cents INTEGER NOT NULL,
                 fuel_cost_cents INTEGER NOT NULL,
                 departed_at INTEGER NOT NULL,
                 arrives_at INTEGER NOT NULL
             );
             INSERT INTO passenger_services(id, name)
                 SELECT id, 'R' || id FROM passenger_services_v2;
             INSERT INTO service_stops(service_id, sequence, station_id)
                 SELECT id, 0, first_station_id FROM passenger_services_v2;
             INSERT INTO service_stops(service_id, sequence, station_id)
                 SELECT id, 1, second_station_id FROM passenger_services_v2;
             INSERT INTO service_lines(service_id, sequence, rail_line_id)
                 SELECT service_id, sequence, rail_line_id FROM service_lines_v2;
             INSERT INTO active_journeys(
                 id, service_id, train_id, origin_station_id, destination_station_id,
                 passengers_carried, fare_cents, operating_revenue_cents,
                 infrastructure_access_fee_cents, fuel_cost_cents, departed_at, arrives_at
             )
                 SELECT id, service_id, train_id, origin_station_id, destination_station_id,
                        passengers_carried, fare_cents, operating_revenue_cents,
                        infrastructure_access_fee_cents, fuel_cost_cents, departed_at, arrives_at
                 FROM active_journeys_v2;
             DROP TABLE active_journeys_v2;
             DROP TABLE service_lines_v2;
             DROP TABLE passenger_services_v2;
             CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);",
        )
        .map_err(|source| db_error("begin v2 to v3 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .pragma_update(None, "user_version", 3_u32)
            .map_err(|source| db_error("write v3 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row("SELECT 1 FROM pragma_foreign_key_check LIMIT 1", [], |row| row.get(0))
            .optional()
            .map_err(|source| db_error("verify v2 to v3 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v2 to v3 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA legacy_alter_table = OFF;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v2 to v3 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA legacy_alter_table = OFF;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}


fn migrate_v3_to_v4(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             BEGIN IMMEDIATE;
             ALTER TABLE active_journeys ADD COLUMN credited_revenue_cents INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE active_journeys ADD COLUMN current_stop_index INTEGER NOT NULL DEFAULT 0;
             CREATE TABLE journey_passenger_groups (
                 journey_id INTEGER NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 passengers INTEGER NOT NULL,
                 fare_cents INTEGER NOT NULL,
                 PRIMARY KEY (journey_id, sequence)
             );
             INSERT INTO journey_passenger_groups(
                 journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents
             )
                 SELECT id, 0, origin_station_id, destination_station_id, passengers_carried, fare_cents
                 FROM active_journeys
                 WHERE passengers_carried > 0;
             UPDATE active_journeys
             SET current_stop_index = CASE
                 WHEN origin_station_id = (
                     SELECT station_id FROM service_stops
                     WHERE service_id = active_journeys.service_id
                     ORDER BY sequence ASC LIMIT 1
                 ) THEN MAX((
                     SELECT COUNT(*) FROM service_stops
                     WHERE service_id = active_journeys.service_id
                 ) - 2, 0)
                 WHEN origin_station_id = (
                     SELECT station_id FROM service_stops
                     WHERE service_id = active_journeys.service_id
                     ORDER BY sequence DESC LIMIT 1
                 ) THEN MIN(1, (
                     SELECT COUNT(*) FROM service_stops
                     WHERE service_id = active_journeys.service_id
                 ) - 1)
                 ELSE 0
             END;",
        )
        .map_err(|source| db_error("begin v3 to v4 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .pragma_update(None, "user_version", 4_u32)
            .map_err(|source| db_error("write v4 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row("SELECT 1 FROM pragma_foreign_key_check LIMIT 1", [], |row| row.get(0))
            .optional()
            .map_err(|source| db_error("verify v3 to v4 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v3 to v4 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v3 to v4 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v4_to_v5(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE region ADD COLUMN registration_code INTEGER NOT NULL DEFAULT 99
                 CHECK (registration_code BETWEEN 10 AND 99);
             ALTER TABLE region ADD COLUMN registration_mark TEXT NOT NULL DEFAULT 'RQ';",
        )
        .map_err(|source| db_error("begin v4 to v5 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT r.name, g.world_seed
                 FROM region r
                 CROSS JOIN game_meta g
                 WHERE r.singleton = 1 AND g.singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| {
                db_error(
                    "read Region registration migration data from",
                    path,
                    source,
                )
            })?;

        if let Some((region_name, world_seed_text)) = existing {
            let world_seed = world_seed_text
                .parse::<u64>()
                .map_err(|_| invalid_value(path, "world seed"))?;
            let registration =
                railway_registration_for_existing_region(&region_name, world_seed);
            connection
                .execute(
                    "UPDATE region
                     SET registration_code = ?1, registration_mark = ?2
                     WHERE singleton = 1",
                    params![
                        i64::from(registration.numeric_code),
                        registration.mark
                    ],
                )
                .map_err(|source| {
                    db_error("write Region registration identity to", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 5_u32)
            .map_err(|source| db_error("write v5 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v4 to v5 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}


fn migrate_v5_to_v6(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE company ADD COLUMN vkm TEXT NOT NULL DEFAULT 'RQ'
                 CHECK (length(vkm) BETWEEN 2 AND 5)
                 CHECK (vkm NOT GLOB '*[^A-Z]*');",
        )
        .map_err(|source| db_error("begin v5 to v6 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let company_name: Option<String> = connection
            .query_row(
                "SELECT name FROM company WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("read Company VKM migration data from", path, source))?;

        if let Some(company_name) = company_name {
            let vkm = VehicleKeeperMark::generated_from_company_name(&company_name);
            connection
                .execute(
                    "UPDATE company SET vkm = ?1 WHERE singleton = 1",
                    params![vkm.as_str()],
                )
                .map_err(|source| db_error("write Company VKM to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", SAVE_VERSION)
            .map_err(|source| db_error("write v6 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v5 to v6 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn clear_state(
    transaction: &Transaction<'_>,
    path: &Path,
    preserve_receipt_history: bool,
) -> Result<(), SaveSlotError> {
    if !preserve_receipt_history {
        transaction
            .execute("DELETE FROM journey_receipts", [])
            .map_err(|source| SaveSlotError::Database {
                action: "clear Journey history from",
                path: path.to_path_buf(),
                source,
            })?;
    }
    transaction.execute_batch(
        "DELETE FROM journey_passenger_groups;
         DELETE FROM active_journeys;
         DELETE FROM origin_destination_demand;
         DELETE FROM service_lines;
         DELETE FROM service_stops;
         DELETE FROM passenger_services;
         DELETE FROM trains;
         DELETE FROM financials;
         DELETE FROM game_rules;
         DELETE FROM company;
         DELETE FROM rail_lines;
         DELETE FROM rail_stations;
         DELETE FROM settlements;
         DELETE FROM region;
         DELETE FROM game_meta;",
    ).map_err(|source| SaveSlotError::Database {
        action: "clear previous state from",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn insert_state(transaction: &Transaction<'_>, state: &GameState, path: &Path) -> Result<(), SaveSlotError> {
    let db = |value: u64, field| to_db_u64(value, field, path);

    transaction.execute(
        "INSERT INTO game_meta(singleton, world_seed, last_processed_at) VALUES(1, ?1, ?2)",
        params![state.world_seed.to_string(), state.last_processed_at.unix_seconds()],
    ).map_err(|source| db_error("write game metadata to", path, source))?;
    transaction.execute(
        "INSERT INTO region(singleton, name, registration_code, registration_mark, population, rail_authority_name)
         VALUES(1, ?1, ?2, ?3, ?4, ?5)",
        params![
            &state.region.name,
            i64::from(state.region.railway_registration.numeric_code),
            &state.region.railway_registration.mark,
            db(state.region.population, "Region Population")?,
            &state.region.rail_authority.name
        ],
    ).map_err(|source| db_error("write Region to", path, source))?;

    for settlement in &state.region.settlements {
        transaction.execute(
            "INSERT INTO settlements(id, name, population) VALUES(?1, ?2, ?3)",
            params![db(settlement.id.get(), "Settlement ID")?, &settlement.name, db(settlement.population, "Settlement Population")?],
        ).map_err(|source| db_error("write Settlements to", path, source))?;
    }
    let network = &state.region.rail_authority.rail_network;
    for station in &network.rail_stations {
        transaction.execute(
            "INSERT INTO rail_stations(id, settlement_id) VALUES(?1, ?2)",
            params![db(station.id.get(), "Rail Station ID")?, db(station.settlement_id.get(), "Settlement ID")?],
        ).map_err(|source| db_error("write Rail Stations to", path, source))?;
    }
    for line in &network.rail_lines {
        transaction.execute(
            "INSERT INTO rail_lines(id, first_station_id, second_station_id, distance_metres) VALUES(?1, ?2, ?3, ?4)",
            params![db(line.id.get(), "Rail Line ID")?, db(line.first_station_id.get(), "Rail Station ID")?, db(line.second_station_id.get(), "Rail Station ID")?, db(line.distance.metres(), "Rail Line distance")?],
        ).map_err(|source| db_error("write Rail Lines to", path, source))?;
    }

    transaction.execute(
        "INSERT INTO company(singleton, name, vkm, funds_cents) VALUES(1, ?1, ?2, ?3)",
        params![
            &state.player_company.name,
            state.player_company.vehicle_keeper_mark.as_str(),
            state.player_company.funds.cents()
        ],
    ).map_err(|source| db_error("write Player Company to", path, source))?;

    for train in &state.player_company.fleet.trains {
        let (status_kind, status_ref_id) = match train.status {
            TrainStatus::Ready { at } => ("ready", at.get()),
            TrainStatus::Travelling { journey_id } => ("travelling", journey_id.get()),
        };
        transaction.execute(
            "INSERT INTO trains(id, status_kind, status_ref_id, model_id, original_purchase_price_cents)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                db(train.id.get(), "Train ID")?, status_kind, db(status_ref_id, "Train status reference")?, train.model_id.as_str(),
                train.original_purchase_price.cents()
            ],
        ).map_err(|source| db_error("write Trains to", path, source))?;
    }

    for service in &state.player_company.passenger_services {
        transaction.execute(
            "INSERT INTO passenger_services(id, name) VALUES(?1, ?2)",
            params![db(service.id.get(), "Passenger Service ID")?, &service.name],
        ).map_err(|source| db_error("write Passenger Services to", path, source))?;
        for (sequence, station_id) in service.stop_station_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_stops(service_id, sequence, station_id) VALUES(?1, ?2, ?3)",
                params![db(service.id.get(), "Passenger Service ID")?, i64::try_from(sequence).unwrap_or(i64::MAX), db(station_id.get(), "Rail Station ID")?],
            ).map_err(|source| db_error("write Passenger Service stops to", path, source))?;
        }
        for (sequence, line_id) in service.rail_line_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_lines(service_id, sequence, rail_line_id) VALUES(?1, ?2, ?3)",
                params![db(service.id.get(), "Passenger Service ID")?, i64::try_from(sequence).unwrap_or(i64::MAX), db(line_id.get(), "Rail Line ID")?],
            ).map_err(|source| db_error("write Passenger Service paths to", path, source))?;
        }
    }

    for demand in &state.origin_destination_demand {
        transaction.execute(
            "INSERT INTO origin_destination_demand(origin_station_id, destination_station_id, waiting_passengers, passenger_arrival_rate_per_hour, fractional_passenger_seconds)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                db(demand.origin_station_id.get(), "Demand origin")?, db(demand.destination_station_id.get(), "Demand destination")?,
                i64::from(demand.waiting_passengers), i64::from(demand.passenger_arrival_rate_per_hour.passengers_per_hour()),
                db(demand.fractional_passenger_seconds, "Demand fractional passenger seconds")?
            ],
        ).map_err(|source| db_error("write Passenger Demand to", path, source))?;
    }

    for journey in &state.active_journeys {
        transaction.execute(
            "INSERT INTO active_journeys(id, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                db(journey.id.get(), "Journey ID")?, db(journey.service_id.get(), "Passenger Service ID")?, db(journey.train_id.get(), "Train ID")?,
                db(journey.origin_station_id.get(), "Journey origin")?, db(journey.destination_station_id.get(), "Journey destination")?, i64::from(journey.passengers_carried),
                journey.fare.cents(), journey.operating_revenue.cents(), journey.credited_revenue.cents(),
                journey.infrastructure_access_fee.cents(), journey.fuel_cost.cents(),
                i64::try_from(journey.current_stop_index).map_err(|_| SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::InvalidValue { field: "Journey current stop index" }),
                })?,
                journey.departed_at.unix_seconds(), journey.arrives_at.unix_seconds()
            ],
        ).map_err(|source| db_error("write active Journeys to", path, source))?;

        for (sequence, group) in journey.passenger_groups.iter().enumerate() {
            transaction.execute(
                "INSERT INTO journey_passenger_groups(journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    db(journey.id.get(), "Journey ID")?,
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    db(group.origin_station_id.get(), "Journey passenger origin")?,
                    db(group.destination_station_id.get(), "Journey passenger destination")?,
                    i64::from(group.passengers),
                    group.fare.cents(),
                ],
            ).map_err(|source| db_error("write Journey passenger groups to", path, source))?;
        }
    }

    transaction.execute(
        "INSERT INTO financials(singleton, operating_revenue_cents, infrastructure_access_fees_cents, fuel_costs_cents) VALUES(1, ?1, ?2, ?3)",
        params![state.financials.operating_revenue.cents(), state.financials.infrastructure_access_fees.cents(), state.financials.fuel_costs.cents()],
    ).map_err(|source| db_error("write financial totals to", path, source))?;
    for receipt in &state.financials.recent_journey_receipts {
        transaction.execute(
            "INSERT OR REPLACE INTO journey_receipts(journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                db(receipt.journey_id.get(), "Journey receipt ID")?, receipt.revenue.cents(), receipt.infrastructure_access_fee.cents(), receipt.fuel_cost.cents(),
                optional_id(receipt.train_id.map(TrainId::get), "Journey receipt Train ID", path)?, receipt.train_model_name.as_deref(),
                optional_id(receipt.origin_station_id.map(RailStationId::get), "Journey receipt origin", path)?,
                optional_id(receipt.destination_station_id.map(RailStationId::get), "Journey receipt destination", path)?,
                receipt.passengers_carried.map(i64::from), receipt.passenger_capacity.map(i64::from), receipt.completed_at.map(UtcSeconds::unix_seconds)
            ],
        ).map_err(|source| db_error("write Journey receipts to", path, source))?;
    }

    let balance = &state.rules.balance;
    transaction.execute(
        "INSERT INTO game_rules(singleton, fare_cents_per_passenger_km, access_fee_cents_per_train_km, starting_company_funds_cents, demand_cap_seconds)
         VALUES(1, ?1, ?2, ?3, ?4)",
        params![
            db(balance.fare_per_passenger_kilometre().cents_per_kilometre(), "fare rate")?,
            db(balance.access_fee_per_train_kilometre().cents_per_kilometre(), "access fee rate")?,
            balance.starting_company_funds().cents(), db(state.rules.demand.cap_duration.seconds(), "demand cap duration")?
        ],
    ).map_err(|source| db_error("write game rules to", path, source))?;
    Ok(())
}

fn load_state(connection: &Connection, path: &Path) -> Result<Option<GameState>, SaveSlotError> {
    let meta = connection
        .query_row(
            "SELECT world_seed, last_processed_at FROM game_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|source| db_error("read game metadata from", path, source))?;
    let Some((world_seed_text, last_processed_at)) = meta else {
        return Ok(None);
    };
    let world_seed = world_seed_text.parse::<u64>().map_err(|_| invalid_value(path, "world seed"))?;

    let (region_name, registration_code, registration_mark, region_population, authority_name):
        (String, i64, String, i64, String) = connection
            .query_row(
                "SELECT name, registration_code, registration_mark, population, rail_authority_name
                 FROM region WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|source| db_error("read Region from", path, source))?;

    let settlements = query_all(connection, "SELECT id, name, population FROM settlements ORDER BY id", path, |row| {
        Ok(Settlement {
            id: SettlementId::new(row_u64(row, 0, "Settlement ID")?),
            name: row.get(1)?,
            population: row_u64(row, 2, "Settlement Population")?,
        })
    })?;
    let rail_stations = query_all(connection, "SELECT id, settlement_id FROM rail_stations ORDER BY id", path, |row| {
        Ok(RailStation {
            id: RailStationId::new(row_u64(row, 0, "Rail Station ID")?),
            settlement_id: SettlementId::new(row_u64(row, 1, "Settlement ID")?),
        })
    })?;
    let rail_lines = query_all(connection, "SELECT id, first_station_id, second_station_id, distance_metres FROM rail_lines ORDER BY id", path, |row| {
        Ok(RailLine {
            id: RailLineId::new(row_u64(row, 0, "Rail Line ID")?),
            first_station_id: RailStationId::new(row_u64(row, 1, "Rail Station ID")?),
            second_station_id: RailStationId::new(row_u64(row, 2, "Rail Station ID")?),
            distance: DistanceMetres::new(row.get(3)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        })
    })?;

    let (company_name, company_vkm, company_funds): (String, String, i64) = connection
        .query_row(
            "SELECT name, vkm, funds_cents FROM company WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|source| db_error("read Player Company from", path, source))?;
    let company_vkm = VehicleKeeperMark::parse(&company_vkm)
        .map_err(|_| invalid_value(path, "Player Company VKM"))?;

    let trains = query_all(connection, "SELECT id, status_kind, status_ref_id, model_id, original_purchase_price_cents FROM trains ORDER BY id", path, |row| {
        let status_kind: String = row.get(1)?;
        let status_ref = row_u64(row, 2, "Train status reference")?;
        let status = match status_kind.as_str() {
            "ready" => TrainStatus::Ready { at: RailStationId::new(status_ref) },
            "travelling" => TrainStatus::Travelling { journey_id: JourneyId::new(status_ref) },
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(Train {
            id: TrainId::new(row_u64(row, 0, "Train ID")?),
            status,
            model_id: TrainModelId::new(row.get::<_, String>(3)?),
            original_purchase_price: Money::from_cents(row.get(4)?),
        })
    })?;

    let mut services = query_all(connection, "SELECT id, name FROM passenger_services ORDER BY id", path, |row| {
        Ok(PassengerService {
            id: ServiceId::new(row_u64(row, 0, "Passenger Service ID")?),
            name: row.get(1)?,
            stop_station_ids: Vec::new(),
            rail_line_ids: Vec::new(),
        })
    })?;
    for service in &mut services {
        let mut stop_statement = connection.prepare("SELECT station_id FROM service_stops WHERE service_id = ?1 ORDER BY sequence")
            .map_err(|source| db_error("prepare Passenger Service stop query for", path, source))?;
        let stop_rows = stop_statement.query_map(params![to_db_u64(service.id.get(), "Passenger Service ID", path)?], |row| row.get::<_, i64>(0))
            .map_err(|source| db_error("read Passenger Service stops from", path, source))?;
        for row in stop_rows {
            let station = row.map_err(|source| db_error("read Passenger Service stop from", path, source))?;
            service.stop_station_ids.push(RailStationId::new(from_db_u64(station, "Rail Station ID").map_err(|field| invalid_value(path, field))?));
        }

        let mut statement = connection.prepare("SELECT rail_line_id FROM service_lines WHERE service_id = ?1 ORDER BY sequence")
            .map_err(|source| db_error("prepare Passenger Service path query for", path, source))?;
        let rows = statement.query_map(params![to_db_u64(service.id.get(), "Passenger Service ID", path)?], |row| row.get::<_, i64>(0))
            .map_err(|source| db_error("read Passenger Service path from", path, source))?;
        for row in rows {
            let line = row.map_err(|source| db_error("read Passenger Service path from", path, source))?;
            service.rail_line_ids.push(RailLineId::new(from_db_u64(line, "Rail Line ID").map_err(|field| invalid_value(path, field))?));
        }
    }

    let demand = query_all(connection, "SELECT origin_station_id, destination_station_id, waiting_passengers, passenger_arrival_rate_per_hour, fractional_passenger_seconds FROM origin_destination_demand ORDER BY origin_station_id, destination_station_id", path, |row| {
        Ok(OriginDestinationDemand {
            origin_station_id: RailStationId::new(row_u64(row, 0, "Demand origin")?),
            destination_station_id: RailStationId::new(row_u64(row, 1, "Demand destination")?),
            waiting_passengers: u32::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            passenger_arrival_rate_per_hour: PassengerArrivalRate::new(row.get(3)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            fractional_passenger_seconds: row_u64(row, 4, "Demand fractional passenger seconds")?,
        })
    })?;

    let mut active_journeys = query_all(connection, "SELECT id, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at FROM active_journeys ORDER BY id", path, |row| {
        Ok(Journey {
            id: JourneyId::new(row_u64(row, 0, "Journey ID")?),
            service_id: ServiceId::new(row_u64(row, 1, "Passenger Service ID")?),
            train_id: TrainId::new(row_u64(row, 2, "Train ID")?),
            origin_station_id: RailStationId::new(row_u64(row, 3, "Journey origin")?),
            destination_station_id: RailStationId::new(row_u64(row, 4, "Journey destination")?),
            passengers_carried: u32::try_from(row.get::<_, i64>(5)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            fare: Money::from_cents(row.get(6)?),
            operating_revenue: Money::from_cents(row.get(7)?),
            credited_revenue: Money::from_cents(row.get(8)?),
            infrastructure_access_fee: Money::from_cents(row.get(9)?),
            fuel_cost: Money::from_cents(row.get(10)?),
            current_stop_index: usize::try_from(row.get::<_, i64>(11)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            passenger_groups: Vec::new(),
            departed_at: UtcSeconds::from_unix_seconds(row.get(12)?),
            arrives_at: UtcSeconds::from_unix_seconds(row.get(13)?),
        })
    })?;
    for journey in &mut active_journeys {
        let mut statement = connection.prepare(
            "SELECT origin_station_id, destination_station_id, passengers, fare_cents
             FROM journey_passenger_groups
             WHERE journey_id = ?1
             ORDER BY sequence"
        ).map_err(|source| db_error("prepare Journey passenger group query for", path, source))?;
        let rows = statement.query_map(
            params![to_db_u64(journey.id.get(), "Journey ID", path)?],
            |row| {
                Ok(JourneyPassengerGroup {
                    origin_station_id: RailStationId::new(row_u64(row, 0, "Journey passenger origin")?),
                    destination_station_id: RailStationId::new(row_u64(row, 1, "Journey passenger destination")?),
                    passengers: u32::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    fare: Money::from_cents(row.get(3)?),
                })
            },
        ).map_err(|source| db_error("read Journey passenger groups from", path, source))?;
        for row in rows {
            journey.passenger_groups.push(
                row.map_err(|source| db_error("read Journey passenger group from", path, source))?
            );
        }
    }

    let (operating_revenue, access_fees, fuel_costs): (i64, i64, i64) = connection
        .query_row("SELECT operating_revenue_cents, infrastructure_access_fees_cents, fuel_costs_cents FROM financials WHERE singleton = 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|source| db_error("read financial totals from", path, source))?;
    let receipt_sql = format!(
        "SELECT journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at
         FROM (
             SELECT journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at
             FROM journey_receipts
             ORDER BY journey_id DESC
             LIMIT {RECENT_RECEIPT_LIMIT}
         )
         ORDER BY journey_id"
    );
    let receipts = query_all(connection, &receipt_sql, path, |row| {
        Ok(JourneyReceipt {
            journey_id: JourneyId::new(row_u64(row, 0, "Journey receipt ID")?),
            revenue: Money::from_cents(row.get(1)?),
            infrastructure_access_fee: Money::from_cents(row.get(2)?),
            fuel_cost: Money::from_cents(row.get(3)?),
            train_id: optional_row_u64(row, 4, "Journey receipt Train ID")?.map(TrainId::new),
            train_model_name: row.get(5)?,
            origin_station_id: optional_row_u64(row, 6, "Journey receipt origin")?.map(RailStationId::new),
            destination_station_id: optional_row_u64(row, 7, "Journey receipt destination")?.map(RailStationId::new),
            passengers_carried: optional_row_u32(row, 8, "Journey receipt passengers")?,
            passenger_capacity: optional_row_u32(row, 9, "Journey receipt passenger capacity")?,
            completed_at: row.get::<_, Option<i64>>(10)?.map(UtcSeconds::from_unix_seconds),
        })
    })?;

    let (fare_rate, access_rate, starting_funds, demand_cap): (i64, i64, i64, i64) = connection
        .query_row("SELECT fare_cents_per_passenger_km, access_fee_cents_per_train_km, starting_company_funds_cents, demand_cap_seconds FROM game_rules WHERE singleton = 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|source| db_error("read game rules from", path, source))?;

    let state = GameState {
        world_seed,
        region: Region {
            name: region_name,
            railway_registration: RailwayRegistration {
                numeric_code: u8::try_from(registration_code)
                    .map_err(|_| invalid_value(path, "Region railway registration code"))?,
                mark: registration_mark,
            },
            population: from_db_u64(region_population, "Region Population").map_err(|field| invalid_value(path, field))?,
            settlements,
            rail_authority: RailAuthority {
                name: authority_name,
                rail_network: RailNetwork { rail_stations, rail_lines },
            },
        },
        player_company: PlayerCompany {
            name: company_name,
            vehicle_keeper_mark: company_vkm,
            funds: Money::from_cents(company_funds),
            fleet: Fleet { trains },
            passenger_services: services,
        },
        origin_destination_demand: demand,
        active_journeys,
        financials: Financials {
            operating_revenue: Money::from_cents(operating_revenue),
            infrastructure_access_fees: Money::from_cents(access_fees),
            fuel_costs: Money::from_cents(fuel_costs),
            recent_journey_receipts: receipts,
        },
        rules: GameRules {
            balance: BalanceConfig::new(
                MoneyPerKilometre::new(fare_rate).map_err(|_| invalid_value(path, "fare rate"))?,
                MoneyPerKilometre::new(access_rate).map_err(|_| invalid_value(path, "access fee rate"))?,
                Money::from_cents(starting_funds),
            ),
            demand: DemandRules {
                cap_duration: DurationSeconds::from_seconds(from_db_u64(demand_cap, "demand cap duration").map_err(|field| invalid_value(path, field))?),
            },
        },
        last_processed_at: UtcSeconds::from_unix_seconds(last_processed_at),
    };
    Ok(Some(state))
}

fn query_all<T>(
    connection: &Connection,
    sql: &str,
    path: &Path,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, SaveSlotError> {
    let mut statement = connection.prepare(sql).map_err(|source| db_error("prepare query for", path, source))?;
    let rows = statement.query_map([], |row| map(row)).map_err(|source| db_error("query", path, source))?;
    rows.map(|row| row.map_err(|source| db_error("decode row from", path, source))).collect()
}

fn to_db_u64(value: u64, field: &'static str, path: &Path) -> Result<i64, SaveSlotError> {
    i64::try_from(value).map_err(|_| invalid_value(path, field))
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize, field: &'static str) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| conversion_error(index, field))
}

fn optional_row_u64(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| u64::try_from(value).map_err(|_| conversion_error(index, field)))
        .transpose()
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

fn optional_id(value: Option<u64>, field: &'static str, path: &Path) -> Result<Option<i64>, SaveSlotError> {
    value.map(|value| to_db_u64(value, field, path)).transpose()
}

fn db_error(action: &'static str, path: &Path, source: rusqlite::Error) -> SaveSlotError {
    SaveSlotError::Database { action, path: path.to_path_buf(), source }
}

fn invalid_value(path: &Path, field: &'static str) -> SaveSlotError {
    SaveSlotError::InvalidSave {
        path: path.to_path_buf(),
        source: Box::new(SaveCodecError::InvalidValue { field }),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySaveEnvelope<T> {
    version: u32,
    state: T,
}

#[derive(Deserialize, Serialize)]
struct LegacyGameStateV1 {
    world_seed: u64,
    region: Region,
    player_company: LegacyPlayerCompanyV1,
    origin_destination_demand: Vec<OriginDestinationDemand>,
    active_journeys: Vec<Journey>,
    financials: Financials,
    rules: LegacyGameRulesV1,
    last_processed_at: UtcSeconds,
}

#[derive(Deserialize, Serialize)]
struct LegacyPlayerCompanyV1 {
    name: String,
    funds: Money,
    fleet: LegacyFleetV1,
    passenger_services: Vec<LegacyPassengerServiceV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyFleetV1 {
    trains: Vec<LegacyTrainV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyTrainV1 {
    id: TrainId,
    status: TrainStatus,
    model_name: String,
    original_purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

#[derive(Deserialize, Serialize)]
struct LegacyPassengerServiceV1 {
    id: ServiceId,
    first_station_id: RailStationId,
    second_station_id: RailStationId,
    rail_line_ids: Vec<RailLineId>,
}

#[derive(Deserialize, Serialize)]
struct LegacyGameRulesV1 {
    balance: LegacyBalanceConfigV1,
    demand: DemandRules,
}

#[derive(Deserialize, Serialize)]
struct LegacyBalanceConfigV1 {
    fare_per_passenger_kilometre: MoneyPerKilometre,
    access_fee_per_train_kilometre: MoneyPerKilometre,
    starting_company_funds: Money,
    diesel_catalogue: Vec<LegacyDieselTrainCatalogueRecordV1>,
}

#[derive(Deserialize, Serialize)]
struct LegacyDieselTrainCatalogueRecordV1 {
    name: String,
    purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

fn decode_legacy_game_state(source: &str) -> Result<GameState, SaveCodecError> {
    const LEGACY_RON_VERSION: u32 = 1;
    let envelope: LegacySaveEnvelope<LegacyGameStateV1> = ron::from_str(source)
        .map_err(|error| SaveCodecError::LegacyDecode(error.to_string()))?;
    if envelope.version != LEGACY_RON_VERSION {
        return Err(SaveCodecError::UnsupportedVersion {
            found: envelope.version,
        });
    }

    let legacy = envelope.state;
    let mut region = legacy.region;
    region.railway_registration =
        railway_registration_for_existing_region(&region.name, legacy.world_seed);
    let trains = legacy
        .player_company
        .fleet
        .trains
        .into_iter()
        .map(|train| {
            let model = train_catalogue()
                .models()
                .iter()
                .find(|model| {
                    model.name() == train.model_name
                        && model.passenger_capacity() == train.passenger_capacity
                        && model.speed() == train.speed
                        && model.fuel_cost_per_kilometre() == train.fuel_cost_per_kilometre
                })
                .ok_or_else(|| SaveCodecError::TrainModelNotFound {
                    model_name: train.model_name.clone(),
                })?;
            Ok(Train {
                id: train.id,
                status: train.status,
                model_id: model.id().clone(),
                original_purchase_price: train.original_purchase_price,
            })
        })
        .collect::<Result<Vec<_>, SaveCodecError>>()?;

    let passenger_services = legacy
        .player_company
        .passenger_services
        .into_iter()
        .map(|service| PassengerService {
            id: service.id,
            name: format!("R{}", service.id.get()),
            stop_station_ids: vec![service.first_station_id, service.second_station_id],
            rail_line_ids: service.rail_line_ids,
        })
        .collect::<Vec<_>>();
    let mut active_journeys = legacy.active_journeys;
    for journey in &mut active_journeys {
        if let Some(service) = passenger_services
            .iter()
            .find(|service| service.id == journey.service_id)
        {
            if journey.origin_station_id == service.destination_station_id().unwrap_or(journey.origin_station_id)
                && journey.destination_station_id == service.origin_station_id().unwrap_or(journey.destination_station_id)
            {
                journey.current_stop_index = service.stop_station_ids.len().saturating_sub(1);
            }
        }
        if journey.passenger_groups.is_empty() && journey.passengers_carried > 0 {
            journey.passenger_groups.push(JourneyPassengerGroup {
                origin_station_id: journey.origin_station_id,
                destination_station_id: journey.destination_station_id,
                passengers: journey.passengers_carried,
                fare: journey.fare,
            });
        }
    }

    let state = GameState {
        world_seed: legacy.world_seed,
        region,
        player_company: PlayerCompany {
            vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name(
                &legacy.player_company.name,
            ),
            name: legacy.player_company.name,
            funds: legacy.player_company.funds,
            fleet: Fleet { trains },
            passenger_services,
        },
        origin_destination_demand: legacy.origin_destination_demand,
        active_journeys,
        financials: legacy.financials,
        rules: GameRules {
            balance: BalanceConfig::new(
                legacy.rules.balance.fare_per_passenger_kilometre,
                legacy.rules.balance.access_fee_per_train_kilometre,
                legacy.rules.balance.starting_company_funds,
            ),
            demand: legacy.rules.demand,
        },
        last_processed_at: legacy.last_processed_at,
    };
    validate_game_state(&state).map_err(SaveCodecError::InvalidState)?;
    Ok(state)
}

/// Why a decoded game cannot safely enter the simulation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveValidationError {
    /// An ID is duplicated within the collection that owns it.
    DuplicateId { kind: &'static str },
    /// An ID is reserved or otherwise not valid for a persisted entity.
    InvalidId { kind: &'static str },
    /// A value is outside the valid domain for a saved game.
    InvalidValue { field: &'static str },
    /// A reference does not resolve within this game state.
    DanglingReference { field: &'static str },
    /// Fields that must agree describe an impossible operating state.
    ImpossibleState { reason: &'static str },
    /// A derived value cannot be calculated in its storage unit.
    Calculation(CalculationError),
}

impl fmt::Display for SaveValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId { kind } => write!(formatter, "duplicate {kind} ID"),
            Self::InvalidId { kind } => write!(formatter, "invalid {kind} ID"),
            Self::InvalidValue { field } => write!(formatter, "invalid saved value for {field}"),
            Self::DanglingReference { field } => {
                write!(
                    formatter,
                    "saved {field} references an entity that does not exist"
                )
            }
            Self::ImpossibleState { reason } => {
                write!(formatter, "impossible saved state: {reason}")
            }
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for SaveValidationError {}

impl From<CalculationError> for SaveValidationError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

pub fn validate_game_state(state: &GameState) -> Result<(), SaveValidationError> {
    validate_rules(state)?;

    let registration = &state.region.railway_registration;
    if !(10..=99).contains(&registration.numeric_code) {
        return Err(SaveValidationError::InvalidValue {
            field: "Region railway registration code",
        });
    }
    if registration.mark.len() != 2
        || !registration
            .mark
            .chars()
            .all(|character| character.is_ascii_uppercase())
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Region railway registration mark",
        });
    }

    if VehicleKeeperMark::parse(state.player_company.vehicle_keeper_mark.as_str()).is_err() {
        return Err(SaveValidationError::InvalidValue {
            field: "Player Company VKM",
        });
    }

    let settlement_ids = unique_ids(
        state
            .region
            .settlements
            .iter()
            .map(|settlement| settlement.id),
        "Settlement",
    )?;
    if state
        .region
        .settlements
        .iter()
        .any(|settlement| settlement.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId { kind: "Settlement" });
    }
    let actual_population =
        state
            .region
            .settlements
            .iter()
            .try_fold(0_u64, |total, settlement| {
                total.checked_add(settlement.population).ok_or(
                    SaveValidationError::ImpossibleState {
                        reason: "Region Population overflows its saved unit",
                    },
                )
            })?;
    if state.region.population != actual_population {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Region Population does not equal its Settlement Populations",
        });
    }

    let network = &state.region.rail_authority.rail_network;
    let station_ids = unique_ids(
        network.rail_stations.iter().map(|station| station.id),
        "Rail Station",
    )?;
    if network
        .rail_stations
        .iter()
        .any(|station| station.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId {
            kind: "Rail Station",
        });
    }
    if network
        .rail_stations
        .iter()
        .any(|station| !settlement_ids.contains(&station.settlement_id))
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Rail Station Settlement",
        });
    }
    let station_settlements = network
        .rail_stations
        .iter()
        .map(|station| station.settlement_id)
        .collect::<HashSet<_>>();
    if station_settlements.len() != network.rail_stations.len() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Settlement has more than one Rail Station",
        });
    }

    let line_ids = unique_ids(network.rail_lines.iter().map(|line| line.id), "Rail Line")?;
    if network.rail_lines.iter().any(|line| line.id.get() == 0) {
        return Err(SaveValidationError::InvalidId { kind: "Rail Line" });
    }
    for line in &network.rail_lines {
        if !station_ids.contains(&line.first_station_id)
            || !station_ids.contains(&line.second_station_id)
        {
            return Err(SaveValidationError::DanglingReference {
                field: "Rail Line endpoint",
            });
        }
        if line.first_station_id == line.second_station_id {
            return Err(SaveValidationError::ImpossibleState {
                reason: "a Rail Line has the same Rail Station at both endpoints",
            });
        }
    }

    let mut service_distances = HashMap::new();
    let service_ids = unique_ids(
        state
            .player_company
            .passenger_services
            .iter()
            .map(|service| service.id),
        "Passenger Service",
    )?;
    if state
        .player_company
        .passenger_services
        .iter()
        .any(|service| service.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId {
            kind: "Passenger Service",
        });
    }
    for service in &state.player_company.passenger_services {
        if service.name.trim().is_empty() {
            return Err(SaveValidationError::InvalidValue {
                field: "Passenger Service name",
            });
        }
        if service
            .stop_station_ids
            .iter()
            .any(|station_id| !station_ids.contains(station_id))
        {
            return Err(SaveValidationError::DanglingReference {
                field: "Passenger Service stop",
            });
        }
        let distance = service_distance(service, network, &line_ids)?;
        service_distances.insert(service.id, distance);
    }

    validate_demand(state, &station_ids)?;
    validate_financials(state)?;

    let train_ids = unique_ids(
        state
            .player_company
            .fleet
            .trains
            .iter()
            .map(|train| train.id),
        "Train",
    )?;
    if state
        .player_company
        .fleet
        .trains
        .iter()
        .any(|train| train.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId { kind: "Train" });
    }
    let journey_ids = unique_ids(
        state.active_journeys.iter().map(|journey| journey.id),
        "Journey",
    )?;
    if state
        .active_journeys
        .iter()
        .any(|journey| journey.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId { kind: "Journey" });
    }

    validate_train_statuses(
        &state.player_company.fleet.trains,
        &station_ids,
        &journey_ids,
    )?;
    for journey in &state.active_journeys {
        validate_journey(
            state,
            journey,
            &train_ids,
            &service_ids,
            &station_ids,
            &service_distances,
        )?;
    }

    Ok(())
}

fn unique_ids<T>(
    ids: impl IntoIterator<Item = T>,
    kind: &'static str,
) -> Result<HashSet<T>, SaveValidationError>
where
    T: Copy + Eq + Hash,
{
    let mut unique = HashSet::new();
    for id in ids {
        if !unique.insert(id) {
            return Err(SaveValidationError::DuplicateId { kind });
        }
    }
    Ok(unique)
}

fn validate_rules(state: &GameState) -> Result<(), SaveValidationError> {
    let balance = &state.rules.balance;
    if balance.starting_company_funds().cents() < 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "starting Company Funds",
        });
    }
    if state.rules.demand.cap_duration.seconds() == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "demand cap duration",
        });
    }
    Ok(())
}

fn service_distance(
    service: &PassengerService,
    network: &RailNetwork,
    line_ids: &HashSet<RailLineId>,
) -> Result<DistanceMetres, SaveValidationError> {
    if service.stop_station_ids.len() < 2 || service.rail_line_ids.is_empty() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Passenger Service must have at least two stops and a Rail Line path",
        });
    }

    if service
        .rail_line_ids
        .iter()
        .any(|rail_line_id| !line_ids.contains(rail_line_id))
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Passenger Service Rail Line",
        });
    }

    let expected_path =
        service_path_for_stops(network, &service.stop_station_ids).map_err(|_| {
            SaveValidationError::ImpossibleState {
                reason: "Passenger Service stops do not form a simple continuous Rail Line path",
            }
        })?;
    if expected_path != service.rail_line_ids {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Passenger Service Rail Lines do not match its ordered stops",
        });
    }

    let total_metres = service
        .rail_line_ids
        .iter()
        .try_fold(0_u64, |total, rail_line_id| {
            let line = network
                .rail_lines
                .iter()
                .find(|line| line.id == *rail_line_id)
                .expect("a validated Rail Line ID resolves in the Rail Network");
            total
                .checked_add(line.distance.metres())
                .ok_or(SaveValidationError::Calculation(
                    CalculationError::Overflow {
                        operation: "Passenger Service path distance",
                    },
                ))
        })?;
    let metres = i64::try_from(total_metres).map_err(|_| {
        SaveValidationError::Calculation(CalculationError::Overflow {
            operation: "Passenger Service path distance",
        })
    })?;
    DistanceMetres::new(metres).map_err(|_| SaveValidationError::InvalidValue {
        field: "Passenger Service path distance",
    })
}

fn validate_demand(
    state: &GameState,
    station_ids: &HashSet<RailStationId>,
) -> Result<(), SaveValidationError> {
    let mut directional_pairs = HashSet::new();
    for demand in &state.origin_destination_demand {
        if !station_ids.contains(&demand.origin_station_id)
            || !station_ids.contains(&demand.destination_station_id)
        {
            return Err(SaveValidationError::DanglingReference {
                field: "origin-destination demand",
            });
        }
        if demand.origin_station_id == demand.destination_station_id {
            return Err(SaveValidationError::ImpossibleState {
                reason: "origin-destination demand has identical endpoints",
            });
        }
        if !directional_pairs.insert((demand.origin_station_id, demand.destination_station_id)) {
            return Err(SaveValidationError::ImpossibleState {
                reason: "duplicate directional Passenger Demand pool",
            });
        }
        let cap = (u128::from(demand.passenger_arrival_rate_per_hour.passengers_per_hour())
            * u128::from(state.rules.demand.cap_duration.seconds())
            / 3_600)
            .min(u128::from(u32::MAX)) as u32;
        if demand.waiting_passengers > cap {
            return Err(SaveValidationError::InvalidValue {
                field: "Waiting Passengers above demand cap",
            });
        }
        if (demand.waiting_passengers == cap && demand.fractional_passenger_seconds != 0)
            || (demand.waiting_passengers < cap && demand.fractional_passenger_seconds >= 3_600)
        {
            return Err(SaveValidationError::InvalidValue {
                field: "fractional Passenger Demand remainder",
            });
        }
    }
    Ok(())
}

fn validate_financials(state: &GameState) -> Result<(), SaveValidationError> {
    if state.player_company.funds.cents() < 0
        || state.financials.operating_revenue.cents() < 0
        || state.financials.infrastructure_access_fees.cents() < 0
        || state.financials.fuel_costs.cents() < 0
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Company Funds or financial total",
        });
    }
    let station_ids = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect::<HashSet<_>>();
    let mut receipt_ids = HashSet::new();
    for receipt in &state.financials.recent_journey_receipts {
        if receipt.journey_id.get() == 0 || !receipt_ids.insert(receipt.journey_id) {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt ID",
            });
        }
        if receipt.revenue.cents() < 0
            || receipt.infrastructure_access_fee.cents() < 0
            || receipt.fuel_cost.cents() < 0
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt amount",
            });
        }
        receipt
            .infrastructure_access_fee
            .checked_add(receipt.fuel_cost)?;

        let metadata_fields_present = [
            receipt.train_id.is_some(),
            receipt.train_model_name.is_some(),
            receipt.origin_station_id.is_some(),
            receipt.destination_station_id.is_some(),
            receipt.passengers_carried.is_some(),
            receipt.passenger_capacity.is_some(),
            receipt.completed_at.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if metadata_fields_present != 0 && metadata_fields_present != 7 {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt operating context",
            });
        }
        if metadata_fields_present == 7 {
            let train_id = receipt.train_id.expect("complete receipt context has Train ID");
            let train_model_name = receipt
                .train_model_name
                .as_deref()
                .expect("complete receipt context has Train model");
            let origin = receipt
                .origin_station_id
                .expect("complete receipt context has origin");
            let destination = receipt
                .destination_station_id
                .expect("complete receipt context has destination");
            let _passengers = receipt
                .passengers_carried
                .expect("complete receipt context has passengers");
            let capacity = receipt
                .passenger_capacity
                .expect("complete receipt context has capacity");
            if train_id.get() == 0
                || train_model_name.trim().is_empty()
                || origin == destination
                || !station_ids.contains(&origin)
                || !station_ids.contains(&destination)
                || capacity == 0
            {
                return Err(SaveValidationError::InvalidValue {
                    field: "Journey receipt operating context",
                });
            }
        }
    }
    Ok(())
}

fn validate_train_statuses(
    trains: &[Train],
    station_ids: &HashSet<RailStationId>,
    journey_ids: &HashSet<crate::model::JourneyId>,
) -> Result<(), SaveValidationError> {
    let mut travelling_journey_ids = HashSet::new();
    for train in trains {
        if train.model_id.as_str().trim().is_empty()
            || model_for_train(train).is_none()
            || train.original_purchase_price.cents() <= 0
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue model reference",
            });
        }
        match train.status {
            TrainStatus::Ready { at } if !station_ids.contains(&at) => {
                return Err(SaveValidationError::DanglingReference {
                    field: "READY Train location",
                });
            }
            TrainStatus::Travelling { journey_id } => {
                if !journey_ids.contains(&journey_id) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "travelling Train Journey",
                    });
                }
                if !travelling_journey_ids.insert(journey_id) {
                    return Err(SaveValidationError::ImpossibleState {
                        reason: "more than one Train is travelling on one Journey",
                    });
                }
            }
            TrainStatus::Ready { .. } => {}
        }
    }
    Ok(())
}

fn validate_journey(
    state: &GameState,
    journey: &Journey,
    train_ids: &HashSet<TrainId>,
    service_ids: &HashSet<ServiceId>,
    station_ids: &HashSet<RailStationId>,
    service_distances: &HashMap<ServiceId, DistanceMetres>,
) -> Result<(), SaveValidationError> {
    if !train_ids.contains(&journey.train_id) {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey Train",
        });
    }
    if !service_ids.contains(&journey.service_id) {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey Passenger Service",
        });
    }
    if !station_ids.contains(&journey.origin_station_id)
        || !station_ids.contains(&journey.destination_station_id)
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey endpoint",
        });
    }

    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == journey.train_id)
        .expect("a validated Journey Train ID resolves in the Fleet");
    if train.status
        != (TrainStatus::Travelling {
            journey_id: journey.id,
        })
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Journey's Train is not travelling on that Journey",
        });
    }
    let train_model = model_for_train(train).ok_or(SaveValidationError::InvalidValue {
        field: "Journey Train catalogue model",
    })?;

    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .expect("a validated Journey Passenger Service ID resolves in the Service Network");
    let Some(service_origin) = service.origin_station_id() else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey references a Passenger Service without an origin",
        });
    };
    let Some(service_destination) = service.destination_station_id() else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey references a Passenger Service without a destination",
        });
    };
    let direction = if journey.origin_station_id == service_origin
        && journey.destination_station_id == service_destination
    {
        1_i32
    } else if journey.origin_station_id == service_destination
        && journey.destination_station_id == service_origin
    {
        -1_i32
    } else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey endpoints do not match its Passenger Service",
        });
    };

    let next_stop_index = match direction {
        1 => journey
            .current_stop_index
            .checked_add(1)
            .filter(|index| *index < service.stop_station_ids.len()),
        -1 => journey.current_stop_index.checked_sub(1),
        _ => None,
    }
    .ok_or(SaveValidationError::ImpossibleState {
        reason: "Journey current Service stop cannot advance",
    })?;

    let distance = service_distances
        .get(&journey.service_id)
        .copied()
        .expect("every validated Passenger Service has a calculated distance");
    let expected_through_fare = state
        .rules
        .balance
        .fare_per_passenger_kilometre()
        .checked_charge(distance)?;
    let expected_access_fee = state
        .rules
        .balance
        .access_fee_per_train_kilometre()
        .checked_charge(distance)?;

    if journey.fare != expected_through_fare
        || journey.infrastructure_access_fee != expected_access_fee
        || journey.fuel_cost.cents() <= 0
        || journey.operating_revenue.cents() < 0
        || journey.credited_revenue.cents() < 0
        || journey.credited_revenue > journey.operating_revenue
        || journey.arrives_at <= journey.departed_at
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey actuals do not match its saved rules and departure snapshot",
        });
    }
    expected_access_fee.checked_add(journey.fuel_cost)?;

    let mut onboard_passengers = 0_u32;
    let mut onboard_revenue = Money::ZERO;
    for group in &journey.passenger_groups {
        if group.passengers == 0
            || !station_ids.contains(&group.origin_station_id)
            || !station_ids.contains(&group.destination_station_id)
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey passenger group",
            });
        }

        let Some(origin_index) = service
            .stop_station_ids
            .iter()
            .position(|station_id| *station_id == group.origin_station_id)
        else {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger origin is not a Service stop",
            });
        };
        let Some(destination_index) = service
            .stop_station_ids
            .iter()
            .position(|station_id| *station_id == group.destination_station_id)
        else {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger destination is not a Service stop",
            });
        };

        let valid_group_direction = if direction > 0 {
            origin_index < destination_index
                && destination_index >= next_stop_index
                && origin_index <= journey.current_stop_index
        } else {
            origin_index > destination_index
                && destination_index <= next_stop_index
                && origin_index >= journey.current_stop_index
        };
        if !valid_group_direction {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger group is not travelling in the Service direction",
            });
        }

        let group_distance = distance_between_service_stops(
            &state.region.rail_authority.rail_network,
            service,
            origin_index,
            destination_index,
        )?;
        let expected_group_fare = state
            .rules
            .balance
            .fare_per_passenger_kilometre()
            .checked_charge(group_distance)?;
        if group.fare != expected_group_fare {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger fare does not match its origin-destination distance",
            });
        }

        onboard_passengers = onboard_passengers
            .checked_add(group.passengers)
            .ok_or(SaveValidationError::Calculation(
                CalculationError::Overflow {
                    operation: "Journey onboard passenger count",
                },
            ))?;
        onboard_revenue = onboard_revenue.checked_add(
            group.fare.checked_mul(u64::from(group.passengers))?,
        )?;
    }

    if onboard_passengers > train_model.passenger_capacity().passengers()
        || journey.passengers_carried < onboard_passengers
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey passenger counts exceed Train capacity or cumulative boardings",
        });
    }
    let expected_booked_revenue = journey.credited_revenue.checked_add(onboard_revenue)?;
    if expected_booked_revenue != journey.operating_revenue {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey booked revenue does not match credited and onboard passengers",
        });
    }

    // Demand pools must exist for every currently onboard OD pair. Their
    // waiting counts need not match the departure snapshot because demand keeps
    // replenishing while the Journey is active.
    for group in &journey.passenger_groups {
        if !state.origin_destination_demand.iter().any(|demand| {
            demand.origin_station_id == group.origin_station_id
                && demand.destination_station_id == group.destination_station_id
        }) {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger group has no saved Passenger Demand",
            });
        }
    }

    Ok(())
}

fn distance_between_service_stops(
    network: &RailNetwork,
    service: &PassengerService,
    first_stop_index: usize,
    second_stop_index: usize,
) -> Result<DistanceMetres, SaveValidationError> {
    let first_station_id = *service
        .stop_station_ids
        .get(first_stop_index)
        .ok_or(SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        })?;
    let second_station_id = *service
        .stop_station_ids
        .get(second_stop_index)
        .ok_or(SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        })?;
    let line_ids = path_between_stations(network, first_station_id, second_station_id).map_err(|_| {
        SaveValidationError::ImpossibleState {
            reason: "Passenger Service stops are not connected by the Rail Network",
        }
    })?;
    let total_metres = line_ids.iter().try_fold(0_u64, |total, rail_line_id| {
        let line = network
            .rail_lines
            .iter()
            .find(|line| line.id == *rail_line_id)
            .ok_or(SaveValidationError::DanglingReference {
                field: "Passenger Service Rail Line",
            })?;
        total
            .checked_add(line.distance.metres())
            .ok_or(SaveValidationError::Calculation(
                CalculationError::Overflow {
                    operation: "Passenger Service stop distance",
                },
            ))
    })?;
    let metres = i64::try_from(total_metres).map_err(|_| {
        SaveValidationError::Calculation(CalculationError::Overflow {
            operation: "Passenger Service stop distance",
        })
    })?;
    DistanceMetres::new(metres).map_err(|_| SaveValidationError::InvalidValue {
        field: "Passenger Service stop distance",
    })
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
            fleet::purchase_train,
            journeys::dispatch_journey,
            services::find_or_create_service,
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
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
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
        let state = active_game();

        slot.save(&state).unwrap();

        assert_eq!(slot.load().unwrap(), Some(state));
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
            .query_row(
                "SELECT COUNT(*) FROM journey_passenger_groups",
                [],
                |row| row.get(0),
            )
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
        let model_id: String = connection
            .query_row("SELECT model_id FROM trains LIMIT 1", [], |row| row.get(0))
            .unwrap();

        assert_eq!(train_count, 1);
        assert_eq!(journey_count, 1);
        assert_eq!(service_stop_count, 2);
        assert!(passenger_group_count > 0);
        assert_eq!(state_blob_table, 0);
        assert_eq!(saved_catalogue_table, 0);
        assert_eq!(model_id, "local-70");
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
                     status_ref_id INTEGER NOT NULL,
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
        let model_id: String = connection
            .query_row("SELECT model_id FROM trains WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        let old_catalogue_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='diesel_catalogue'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let journey_train_id: i64 = connection
            .query_row("SELECT train_id FROM active_journeys WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        let service_name: String = connection
            .query_row("SELECT name FROM passenger_services WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        let service_stop_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM service_stops WHERE service_id = 1", [], |row| row.get(0))
            .unwrap();
        let foreign_key_violations: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))
            .unwrap();
        let (registration_code, registration_mark): (i64, String) = connection
            .query_row(
                "SELECT registration_code, registration_mark FROM region WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let company_vkm: String = connection
            .query_row("SELECT vkm FROM company WHERE singleton = 1", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, SAVE_VERSION);
        assert_eq!(model_id, "local-70");
        assert_eq!(old_catalogue_exists, 0);
        assert_eq!(journey_train_id, 1);
        assert_eq!(service_name, "R1");
        assert_eq!(service_stop_count, 2);
        assert_eq!(foreign_key_violations, 0);
        assert_eq!(registration_code, 67);
        assert_eq!(registration_mark, "VA");
        assert_eq!(company_vkm, "OMP");
    }

    #[test]
    fn legacy_ron_decoder_accepts_the_previous_versioned_envelope() {
        let state = active_game();
        let legacy_trains = state
            .player_company
            .fleet
            .trains
            .iter()
            .map(|train| {
                let model = model_for_train(train).unwrap();
                LegacyTrainV1 {
                    id: train.id,
                    status: train.status,
                    model_name: model.name().to_owned(),
                    original_purchase_price: train.original_purchase_price,
                    passenger_capacity: model.passenger_capacity(),
                    speed: model.speed(),
                    fuel_cost_per_kilometre: model.fuel_cost_per_kilometre(),
                }
            })
            .collect();
        let legacy_catalogue = train_catalogue()
            .models()
            .iter()
            .map(|model| LegacyDieselTrainCatalogueRecordV1 {
                name: model.name().to_owned(),
                purchase_price: model.purchase_price(),
                passenger_capacity: model.passenger_capacity(),
                speed: model.speed(),
                fuel_cost_per_kilometre: model.fuel_cost_per_kilometre(),
            })
            .collect();
        let legacy = LegacyGameStateV1 {
            world_seed: state.world_seed,
            region: state.region.clone(),
            player_company: LegacyPlayerCompanyV1 {
                name: state.player_company.name.clone(),
                funds: state.player_company.funds,
                fleet: LegacyFleetV1 {
                    trains: legacy_trains,
                },
                passenger_services: state
                    .player_company
                    .passenger_services
                    .iter()
                    .map(|service| LegacyPassengerServiceV1 {
                        id: service.id,
                        first_station_id: service.origin_station_id().unwrap(),
                        second_station_id: service.destination_station_id().unwrap(),
                        rail_line_ids: service.rail_line_ids.clone(),
                    })
                    .collect(),
            },
            origin_destination_demand: state.origin_destination_demand.clone(),
            active_journeys: state.active_journeys.clone(),
            financials: state.financials.clone(),
            rules: LegacyGameRulesV1 {
                balance: LegacyBalanceConfigV1 {
                    fare_per_passenger_kilometre: state
                        .rules
                        .balance
                        .fare_per_passenger_kilometre(),
                    access_fee_per_train_kilometre: state
                        .rules
                        .balance
                        .access_fee_per_train_kilometre(),
                    starting_company_funds: state.rules.balance.starting_company_funds(),
                    diesel_catalogue: legacy_catalogue,
                },
                demand: state.rules.demand.clone(),
            },
            last_processed_at: state.last_processed_at,
        };
        let source = ron::ser::to_string(&LegacySaveEnvelope {
            version: 1,
            state: legacy,
        })
        .unwrap();

        assert_eq!(decode_legacy_game_state(&source).unwrap(), state);
    }
}

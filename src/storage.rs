//! SQLite persistence for RailQ.
//!
//! Mutable game state is stored in normalized tables rather than one growing
//! RON document. The application still works with a validated [`GameState`]
//! at its boundary; this module is responsible for reconstructing that state
//! transactionally and checking every simulation invariant before publishing
//! it to the rest of the program.

mod migrations;
mod schema;
mod state_io;
mod validation;

pub use self::validation::{SaveValidationError, validate_game_state};

use self::{
    migrations::ensure_schema,
    schema::SCHEMA,
    state_io::{clear_state, insert_state, load_state},
};

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
    catalog::{TrainModel, model_for_train, train_catalogue},
    model::{
        BulletinCategory, BulletinEntry, CalculationError, ConstructionDifficulty, DemandRules,
        DistanceMetres, DurationSeconds, Electrification, EuropeanVehicleNumber, Financials, Fleet,
        GameRules, GameState,
        InfrastructureProject, InfrastructureProjectFunding, InfrastructureProjectId,
        InfrastructureProjectKind, InfrastructureProjectStatus, InfrastructureProjectTimeline,
        Journey, JourneyId, JourneyPassengerGroup, JourneyReceipt, MarketMaturity, Money,
        MoneyPerKilometre, OriginDestinationDemand, PassengerArrivalRate, PassengerCapacity,
        PassengerService,
        PlannedRailLine, PlannedRailStation, PlayerCompany, RailAuthority, RailAuthorityFinances,
        RailLine, RailLineId, RailNetwork, RailStation, RailStationId, RailwayRegistration, Region,
        ServiceId, Settlement, SettlementId, SpeedKilometresPerHour, SpeedMetresPerSecond,
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

fn migrate_v22_to_v23(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v22 to v23 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let finances_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_authority_finances'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Authority finances during v23 migration in", path, source))?;
        if finances_exist != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE rail_authority_finances
                     ADD COLUMN next_fiscal_period_at INTEGER;",
                )
                .map_err(|source| {
                    db_error("add Rail Authority fiscal calendar to", path, source)
                })?;
        }

        let game_meta_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'game_meta'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect game metadata during v23 migration in",
                    path,
                    source,
                )
            })?;
        if finances_exist != 0 && game_meta_exists != 0 {
            connection
                .execute_batch(
                    "UPDATE rail_authority_finances
                     SET next_fiscal_period_at = (
                         SELECT last_processed_at + 21600
                         FROM game_meta
                         WHERE singleton = 1
                     )
                     WHERE singleton = 1;",
                )
                .map_err(|source| {
                    db_error("seed Rail Authority fiscal calendar in", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 23_u32)
            .map_err(|source| db_error("write v23 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v22 to v23 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v23_to_v24(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v23 to v24 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let finances_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_authority_finances'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Authority finances during v24 migration in", path, source))?;
        let game_meta_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'game_meta'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect game metadata during v24 migration in",
                    path,
                    source,
                )
            })?;

        if finances_exist != 0 && game_meta_exists != 0 {
            connection
                .execute_batch(
                    "UPDATE rail_authority_finances
                     SET next_fiscal_period_at = (
                         SELECT ((last_processed_at / 86400) + 1) * 86400
                         FROM game_meta
                         WHERE singleton = 1
                     )
                     WHERE singleton = 1;",
                )
                .map_err(|source| {
                    db_error(
                        "align Authority fiscal calendar to UTC midnight in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 24_u32)
            .map_err(|source| db_error("write v24 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v23 to v24 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v24_to_v25(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v24 to v25 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let demand_table_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'origin_destination_demand'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect Passenger Demand during v25 migration in",
                    path,
                    source,
                )
            })?;

        if demand_table_exists != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE origin_destination_demand
                     ADD COLUMN market_maturity_basis_points INTEGER NOT NULL DEFAULT 10000
                     CHECK (market_maturity_basis_points BETWEEN 0 AND 10000);",
                )
                .map_err(|source| {
                    db_error(
                        "add Passenger Demand maturity during v25 migration in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 25_u32)
            .map_err(|source| db_error("write v25 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v24 to v25 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v25_to_v26(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v25 to v26 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let projects_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'infrastructure_projects'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect infrastructure projects during v26 migration in",
                    path,
                    source,
                )
            })?;

        if projects_exist != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE infrastructure_projects
                     ADD COLUMN reconsideration_count INTEGER NOT NULL DEFAULT 0
                     CHECK (reconsideration_count >= 0);",
                )
                .map_err(|source| {
                    db_error(
                        "add project reconsideration count during v26 migration in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 26_u32)
            .map_err(|source| db_error("write v26 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v25 to v26 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v26_to_v27(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v26 to v27 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS bulletin_entries (
                     sequence INTEGER PRIMARY KEY,
                     occurred_at INTEGER NOT NULL,
                     category TEXT NOT NULL CHECK (category IN ('local', 'authority', 'construction', 'network')),
                     headline TEXT NOT NULL,
                     detail TEXT NOT NULL
                 );",
            )
            .map_err(|source| db_error("add Railway Bulletin history to", path, source))?;
        connection
            .pragma_update(None, "user_version", 27_u32)
            .map_err(|source| db_error("write v27 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v26 to v27 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
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
                 status_ref_id TEXT NOT NULL,
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
            let model = catalogue_model_for_legacy_signature(
                &model_name,
                passenger_capacity,
                speed,
                fuel_rate,
            )
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
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
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
                 service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 station_id TEXT NOT NULL REFERENCES rail_stations(id),
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE service_lines (
                 service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
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
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
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
                 journey_id TEXT NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
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
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
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
                db_error("read Region registration migration data from", path, source)
            })?;

        if let Some((region_name, world_seed_text)) = existing {
            let world_seed = world_seed_text
                .parse::<u64>()
                .map_err(|_| invalid_value(path, "world seed"))?;
            let registration = railway_registration_for_existing_region(&region_name, world_seed);
            connection
                .execute(
                    "UPDATE region
                     SET registration_code = ?1, registration_mark = ?2
                     WHERE singleton = 1",
                    params![i64::from(registration.numeric_code), registration.mark],
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
            .query_row("SELECT name FROM company WHERE singleton = 1", [], |row| {
                row.get(0)
            })
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
            .pragma_update(None, "user_version", 6_u32)
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

fn migrate_v6_to_v7(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE company ADD COLUMN next_train_id INTEGER NOT NULL DEFAULT 1
                 CHECK (next_train_id > 0);
             ALTER TABLE trains ADD COLUMN evn TEXT;",
        )
        .map_err(|source| db_error("begin v6 to v7 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let registration_code: Option<i64> = connection
            .query_row(
                "SELECT registration_code FROM region WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("read EVN Region registration from", path, source))?;

        let next_train_id: i64 = connection
            .query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM trains", [], |row| {
                row.get(0)
            })
            .map_err(|source| {
                db_error(
                    "calculate next Train ID for v7 migration from",
                    path,
                    source,
                )
            })?;
        connection
            .execute(
                "UPDATE company SET next_train_id = ?1 WHERE singleton = 1",
                params![next_train_id],
            )
            .map_err(|source| db_error("write next Train ID to", path, source))?;

        if let Some(registration_code) = registration_code {
            let registration_code = u8::try_from(registration_code)
                .map_err(|_| invalid_value(path, "Region registration code"))?;
            let mut statement = connection
                .prepare("SELECT id, model_id FROM trains ORDER BY model_id, id")
                .map_err(|source| {
                    db_error("read v6 Trains for EVN migration from", path, source)
                })?;
            let mut rows = statement.query([]).map_err(|source| {
                db_error("read v6 Trains for EVN migration from", path, source)
            })?;

            let mut migrated = Vec::new();
            let mut next_units: HashMap<String, u16> = HashMap::new();
            while let Some(row) = rows.next().map_err(|source| {
                db_error("read v6 Train row for EVN migration from", path, source)
            })? {
                let train_id = from_db_u64(
                    row.get::<_, i64>(0)
                        .map_err(|source| db_error("decode v6 Train ID from", path, source))?,
                    "Train ID",
                )
                .map_err(|field| invalid_value(path, field))?;
                let model_id: String = row
                    .get(1)
                    .map_err(|source| db_error("decode v6 Train model from", path, source))?;
                let model = catalogue_model_for_persisted_id(&model_id).ok_or_else(|| {
                    SaveSlotError::InvalidSave {
                        path: path.to_path_buf(),
                        source: Box::new(SaveCodecError::TrainModelNotFound {
                            model_name: model_id.clone(),
                        }),
                    }
                })?;
                let unit_number = next_units.entry(model_id.clone()).or_insert(1);
                if *unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER {
                    return Err(invalid_value(path, "EVN unit number"));
                }
                let evn = EuropeanVehicleNumber::generate(
                    model.evn_type_code(),
                    registration_code,
                    model.evn_series_code(),
                    *unit_number,
                )
                .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
                migrated.push((train_id, evn));
                *unit_number += 1;
            }
            drop(rows);
            drop(statement);

            for (train_id, evn) in migrated {
                connection
                    .execute(
                        "UPDATE trains SET evn = ?1 WHERE id = ?2",
                        params![evn.as_str(), to_db_u64(train_id, "Train ID", path)?],
                    )
                    .map_err(|source| db_error("write migrated Train EVN to", path, source))?;
            }
        }

        let missing_evn: i64 = connection
            .query_row("SELECT COUNT(*) FROM trains WHERE evn IS NULL", [], |row| {
                row.get(0)
            })
            .map_err(|source| db_error("verify migrated Train EVNs in", path, source))?;
        if missing_evn != 0 {
            return Err(invalid_value(path, "European Vehicle Number"));
        }

        connection
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_trains_evn ON trains(evn)
                 WHERE evn IS NOT NULL;",
            )
            .map_err(|source| db_error("index migrated Train EVNs in", path, source))?;
        connection
            .pragma_update(None, "user_version", 7_u32)
            .map_err(|source| db_error("write v7 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v6 to v7 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v7_to_v8(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE train_model_sequences (
                 model_id TEXT PRIMARY KEY,
                 next_unit_number INTEGER NOT NULL CHECK (next_unit_number BETWEEN 1 AND 1000)
             );",
        )
        .map_err(|source| db_error("begin v7 to v8 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let registration_code: i64 = connection
            .query_row(
                "SELECT registration_code FROM region WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error("read Region registration for v8 EVNs from", path, source)
            })?;
        let registration_code = u8::try_from(registration_code)
            .map_err(|_| invalid_value(path, "Region registration code"))?;

        let mut statement = connection
            .prepare("SELECT id, model_id FROM trains ORDER BY model_id, id")
            .map_err(|source| db_error("read v7 Trains for EVN refinement from", path, source))?;
        let mut rows = statement
            .query([])
            .map_err(|source| db_error("read v7 Trains for EVN refinement from", path, source))?;

        let mut next_units: HashMap<String, u16> = HashMap::new();
        let mut migrated = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|source| db_error("read v7 Train row for EVN refinement from", path, source))?
        {
            let train_id = from_db_u64(
                row.get::<_, i64>(0)
                    .map_err(|source| db_error("decode v7 Train ID from", path, source))?,
                "Train ID",
            )
            .map_err(|field| invalid_value(path, field))?;
            let model_id: String = row
                .get(1)
                .map_err(|source| db_error("decode v7 Train model from", path, source))?;
            let model = catalogue_model_for_persisted_id(&model_id).ok_or_else(|| {
                SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::TrainModelNotFound {
                        model_name: model_id.clone(),
                    }),
                }
            })?;

            let unit_number = next_units.entry(model_id.clone()).or_insert(1);
            if *unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER {
                return Err(invalid_value(path, "EVN unit number"));
            }
            let evn = EuropeanVehicleNumber::generate(
                model.evn_type_code(),
                registration_code,
                model.evn_series_code(),
                *unit_number,
            )
            .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            migrated.push((train_id, evn));
            *unit_number += 1;
        }
        drop(rows);
        drop(statement);

        for (train_id, evn) in migrated {
            connection
                .execute(
                    "UPDATE trains SET evn = ?1 WHERE id = ?2",
                    params![evn.as_str(), to_db_u64(train_id, "Train ID", path)?],
                )
                .map_err(|source| db_error("write refined Train EVN to", path, source))?;
        }

        for (model_id, next_unit_number) in next_units {
            connection
                .execute(
                    "INSERT INTO train_model_sequences(model_id, next_unit_number) VALUES(?1, ?2)",
                    params![model_id, i64::from(next_unit_number)],
                )
                .map_err(|source| db_error("write EVN model sequence to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 8_u32)
            .map_err(|source| db_error("write v8 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v7 to v8 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v8_to_v9(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE trains ADD COLUMN nickname TEXT
                 CHECK (nickname IS NULL OR length(trim(nickname)) BETWEEN 1 AND 32);",
        )
        .map_err(|source| db_error("begin v8 to v9 migration for", path, source))?;

    let migration = connection
        .pragma_update(None, "user_version", 9_u32)
        .map_err(|source| db_error("write v9 schema version to", path, source));

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v8 to v9 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v9_to_v10(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v9 to v10 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let mut statement = connection
            .prepare(
                "SELECT CAST(id AS TEXT), evn, model_id
                 FROM trains
                 WHERE model_id IN ('local-70', 'express-120')
                 ORDER BY id",
            )
            .map_err(|source| {
                db_error("read v9 Trains for catalogue migration from", path, source)
            })?;
        let mut rows = statement.query([]).map_err(|source| {
            db_error("read v9 Trains for catalogue migration from", path, source)
        })?;
        let mut migrated_trains = Vec::new();
        while let Some(row) = rows.next().map_err(|source| {
            db_error(
                "read v9 Train row for catalogue migration from",
                path,
                source,
            )
        })? {
            let train_id: String = row
                .get(0)
                .map_err(|source| db_error("decode v9 Train ID from", path, source))?;
            let old_evn: String = row
                .get(1)
                .map_err(|source| db_error("decode v9 Train EVN from", path, source))?;
            let old_model_id: String = row
                .get(2)
                .map_err(|source| db_error("decode v9 Train model from", path, source))?;
            let replacement = catalogue_model_for_persisted_id(&old_model_id).ok_or_else(|| {
                SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::TrainModelNotFound {
                        model_name: old_model_id.clone(),
                    }),
                }
            })?;
            let old_evn = EuropeanVehicleNumber::parse(&old_evn)
                .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            let replacement_evn = EuropeanVehicleNumber::generate(
                replacement.evn_type_code(),
                old_evn.registration_code(),
                replacement.evn_series_code(),
                old_evn.unit_number(),
            )
            .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            migrated_trains.push((
                train_id,
                replacement_evn,
                replacement.id().as_str().to_owned(),
            ));
        }
        drop(rows);
        drop(statement);

        for (train_id, evn, model_id) in migrated_trains {
            connection
                .execute(
                    "UPDATE trains SET evn = ?1, model_id = ?2 WHERE CAST(id AS TEXT) = ?3",
                    params![evn.as_str(), model_id, train_id],
                )
                .map_err(|source| db_error("write migrated v10 Train to", path, source))?;
        }

        for (old_model_id, new_model_id) in [
            ("local-70", "helvetra-r70"),
            ("express-120", "veltrian-d121"),
        ] {
            let old_next_unit: Option<i64> = connection
                .query_row(
                    "SELECT next_unit_number FROM train_model_sequences WHERE model_id = ?1",
                    params![old_model_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("read legacy EVN sequence from", path, source))?;
            let Some(old_next_unit) = old_next_unit else {
                continue;
            };
            let new_next_unit: Option<i64> = connection
                .query_row(
                    "SELECT next_unit_number FROM train_model_sequences WHERE model_id = ?1",
                    params![new_model_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("read replacement EVN sequence from", path, source))?;
            let merged_next_unit =
                new_next_unit.map_or(old_next_unit, |current| current.max(old_next_unit));

            connection
                .execute(
                    "DELETE FROM train_model_sequences WHERE model_id = ?1",
                    params![old_model_id],
                )
                .map_err(|source| db_error("remove legacy EVN sequence from", path, source))?;
            connection
                .execute(
                    "INSERT INTO train_model_sequences(model_id, next_unit_number)
                     VALUES(?1, ?2)
                     ON CONFLICT(model_id) DO UPDATE SET next_unit_number = excluded.next_unit_number",
                    params![new_model_id, merged_next_unit],
                )
                .map_err(|source| db_error("write replacement EVN sequence to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 10_u32)
            .map_err(|source| db_error("write v10 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v9 to v10 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v10_to_v11(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v10 to v11 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE rail_lines ADD COLUMN speed_limit_kmh INTEGER NOT NULL DEFAULT 70 CHECK (speed_limit_kmh > 0);
                 ALTER TABLE rail_lines ADD COLUMN track_count INTEGER NOT NULL DEFAULT 1 CHECK (track_count > 0);
                 ALTER TABLE rail_lines ADD COLUMN electrification TEXT NOT NULL DEFAULT 'none' CHECK (electrification IN ('none', 'electric'));
                 ALTER TABLE rail_lines ADD COLUMN construction_difficulty TEXT NOT NULL DEFAULT 'moderate' CHECK (construction_difficulty IN ('low', 'moderate', 'high'));",
            )
            .map_err(|source| db_error("add Rail Line capabilities to", path, source))?;
        connection
            .pragma_update(None, "user_version", 11_u32)
            .map_err(|source| db_error("write v11 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v10 to v11 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v11_to_v12(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v11 to v12 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE region ADD COLUMN next_infrastructure_project_id INTEGER NOT NULL DEFAULT 1 CHECK (next_infrastructure_project_id > 0);
                 CREATE TABLE infrastructure_projects (
                     id INTEGER PRIMARY KEY,
                     kind TEXT NOT NULL CHECK (kind IN ('new_line', 'speed_upgrade', 'double_tracking', 'electrification', 'renewal', 'station_upgrade')),
                     status TEXT NOT NULL CHECK (status IN ('requested', 'under_review', 'proposed', 'approved', 'deferred', 'funding', 'scheduled', 'construction', 'open', 'cancelled')),
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
                     target_speed_limit_kmh INTEGER CHECK (target_speed_limit_kmh IS NULL OR target_speed_limit_kmh > 0),
                     target_track_count INTEGER CHECK (target_track_count IS NULL OR target_track_count > 0)
                 );
                 CREATE TABLE infrastructure_project_rail_lines (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_rail_stations (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     rail_station_id TEXT NOT NULL REFERENCES rail_stations(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_planned_stations (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     station_id INTEGER NOT NULL UNIQUE,
                     settlement_id INTEGER NOT NULL REFERENCES settlements(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_planned_lines (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     line_id INTEGER NOT NULL UNIQUE,
                     first_station_id INTEGER NOT NULL,
                     second_station_id INTEGER NOT NULL,
                     distance_metres INTEGER NOT NULL CHECK (distance_metres > 0),
                     speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
                     track_count INTEGER NOT NULL CHECK (track_count > 0),
                     electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
                     construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high')),
                     PRIMARY KEY (project_id, sequence)
                 );",
            )
            .map_err(|source| db_error("add infrastructure project persistence to", path, source))?;
        connection
            .pragma_update(None, "user_version", 12_u32)
            .map_err(|source| db_error("write v12 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v11 to v12 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v12_to_v13(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v12 to v13 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0)
                 );
                 INSERT INTO rail_authority_finances(
                     singleton, treasury_cents, maintenance_reserve_cents,
                     committed_investment_cents, carried_over_funds_cents
                 ) VALUES(1, 0, 0, 0, 0);",
            )
            .map_err(|source| db_error("add Rail Authority finances to", path, source))?;
        connection
            .pragma_update(None, "user_version", 13_u32)
            .map_err(|source| db_error("write v13 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v12 to v13 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v13_to_v14(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v13 to v14 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let allocation = crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents();
        connection
            .execute(
                "ALTER TABLE rail_authority_finances
                 ADD COLUMN regional_public_allocation_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (regional_public_allocation_cents >= 0)",
                [],
            )
            .map_err(|source| db_error("add regional public allocation to", path, source))?;
        connection
            .execute(
                "UPDATE rail_authority_finances
                 SET regional_public_allocation_cents = ?1,
                     treasury_cents = treasury_cents + ?1
                 WHERE singleton = 1",
                params![allocation],
            )
            .map_err(|source| db_error("seed regional public allocation in", path, source))?;
        connection
            .pragma_update(None, "user_version", 14_u32)
            .map_err(|source| db_error("write v14 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v13 to v14 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v14_to_v15(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v14 to v15 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let rail_lines_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_lines'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error("inspect Rail Lines during v15 migration in", path, source)
            })?;

        if rail_lines_exist != 0 {
            let rate = crate::model::PROVISIONAL_MAINTENANCE_RESERVE_PER_TRACK_KILOMETRE
                .cents_per_kilometre();
            let rate = i64::try_from(rate)
                .expect("the provisional maintenance reserve rate fits SQLite INTEGER");
            connection
                .execute(
                    "UPDATE rail_authority_finances
                     SET maintenance_reserve_cents = MIN(
                         treasury_cents - committed_investment_cents,
                         COALESCE((
                             SELECT SUM((((distance_metres * ?1) + 999) / 1000) * track_count)
                             FROM rail_lines
                         ), 0)
                     )
                     WHERE singleton = 1",
                    params![rate],
                )
                .map_err(|source| {
                    db_error("seed infrastructure maintenance reserve in", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 15_u32)
            .map_err(|source| db_error("write v15 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v14 to v15 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v15_to_v16(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v15 to v16 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE rail_authority_finances
                 ADD COLUMN infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (infrastructure_access_fee_revenue_cents >= 0);",
            )
            .map_err(|source| db_error("add Authority access-fee revenue to", path, source))?;

        let financials_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'financials'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect financial history during v16 migration in",
                    path,
                    source,
                )
            })?;
        if financials_exist != 0 {
            let historical_access_fees: i64 = connection
                .query_row(
                    "SELECT COALESCE(infrastructure_access_fees_cents, 0)
                     FROM financials WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| {
                    db_error(
                        "read historical access fees during v16 migration from",
                        path,
                        source,
                    )
                })?
                .unwrap_or(0);
            connection
                .execute(
                    "UPDATE rail_authority_finances
                     SET infrastructure_access_fee_revenue_cents = ?1,
                         treasury_cents = treasury_cents + ?1
                     WHERE singleton = 1",
                    params![historical_access_fees],
                )
                .map_err(|source| {
                    db_error(
                        "credit historical access fees to Rail Authority in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 16_u32)
            .map_err(|source| db_error("write v16 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v15 to v16 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
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

fn migrate_v16_to_v17(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(V16_IDENTITY_TABLES_COMPAT_SCHEMA)
        .map_err(|source| db_error("prepare v16 UUID migration tables in", path, source))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE region RENAME TO region_v16;
             ALTER TABLE settlements RENAME TO settlements_v16;
             ALTER TABLE rail_stations RENAME TO rail_stations_v16;
             ALTER TABLE rail_lines RENAME TO rail_lines_v16;
             ALTER TABLE infrastructure_projects RENAME TO infrastructure_projects_v16;
             ALTER TABLE infrastructure_project_rail_lines RENAME TO infrastructure_project_rail_lines_v16;
             ALTER TABLE infrastructure_project_rail_stations RENAME TO infrastructure_project_rail_stations_v16;
             ALTER TABLE infrastructure_project_planned_stations RENAME TO infrastructure_project_planned_stations_v16;
             ALTER TABLE infrastructure_project_planned_lines RENAME TO infrastructure_project_planned_lines_v16;
             ALTER TABLE company RENAME TO company_v16;
             ALTER TABLE trains RENAME TO trains_v16;
             ALTER TABLE passenger_services RENAME TO passenger_services_v16;
             ALTER TABLE service_stops RENAME TO service_stops_v16;
             ALTER TABLE service_lines RENAME TO service_lines_v16;
             ALTER TABLE origin_destination_demand RENAME TO origin_destination_demand_v16;
             ALTER TABLE active_journeys RENAME TO active_journeys_v16;
             ALTER TABLE journey_passenger_groups RENAME TO journey_passenger_groups_v16;
             ALTER TABLE journey_receipts RENAME TO journey_receipts_v16;",
        )
        .map_err(|source| db_error("begin UUID migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(SCHEMA)
            .map_err(|source| db_error("create UUID schema in", path, source))?;

        connection
            .execute_batch(
                r#"
                INSERT INTO region(singleton, name, registration_code, registration_mark, population, rail_authority_name)
                SELECT singleton, name, registration_code, registration_mark, population, rail_authority_name
                FROM region_v16;

                INSERT INTO settlements(id, sequence, name, population)
                SELECT printf('00000001-0000-4000-8000-%012x', id), id, name, population
                FROM settlements_v16;

                INSERT INTO rail_stations(id, sequence, settlement_id)
                SELECT printf('00000002-0000-4000-8000-%012x', id), id,
                       printf('00000001-0000-4000-8000-%012x', settlement_id)
                FROM rail_stations_v16;

                INSERT INTO rail_lines(id, sequence, first_station_id, second_station_id, distance_metres,
                                       speed_limit_kmh, track_count, electrification, construction_difficulty)
                SELECT printf('00000003-0000-4000-8000-%012x', id), id,
                       printf('00000002-0000-4000-8000-%012x', first_station_id),
                       printf('00000002-0000-4000-8000-%012x', second_station_id),
                       distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                FROM rail_lines_v16;

                INSERT INTO infrastructure_projects(
                    id, sequence, kind, status, requested_at, review_started_at, proposed_at, approved_at,
                    funding_completed_at, scheduled_start_at, construction_started_at,
                    planned_completion_at, completed_at, deferred_at, cancelled_at,
                    target_speed_limit_kmh, target_track_count
                )
                SELECT printf('00000004-0000-4000-8000-%012x', id), id, kind, status, requested_at,
                       review_started_at, proposed_at, approved_at, funding_completed_at,
                       scheduled_start_at, construction_started_at, planned_completion_at,
                       completed_at, deferred_at, cancelled_at, target_speed_limit_kmh,
                       target_track_count
                FROM infrastructure_projects_v16;

                INSERT INTO infrastructure_project_rail_lines(project_id, sequence, rail_line_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', rail_line_id)
                FROM infrastructure_project_rail_lines_v16;

                INSERT INTO infrastructure_project_rail_stations(project_id, sequence, rail_station_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', rail_station_id)
                FROM infrastructure_project_rail_stations_v16;

                INSERT INTO infrastructure_project_planned_stations(project_id, sequence, station_id, settlement_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', station_id),
                       printf('00000001-0000-4000-8000-%012x', settlement_id)
                FROM infrastructure_project_planned_stations_v16;

                INSERT INTO infrastructure_project_planned_lines(
                    project_id, sequence, line_id, first_station_id, second_station_id,
                    distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                )
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', line_id),
                       printf('00000002-0000-4000-8000-%012x', first_station_id),
                       printf('00000002-0000-4000-8000-%012x', second_station_id),
                       distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                FROM infrastructure_project_planned_lines_v16;

                INSERT INTO company(singleton, name, vkm, funds_cents, next_train_display_number)
                SELECT singleton, name, vkm, funds_cents, next_train_id FROM company_v16;

                INSERT INTO trains(id, sequence, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents)
                SELECT printf('00000005-0000-4000-8000-%012x', id), id, evn, nickname, status_kind,
                       CASE status_kind
                           WHEN 'ready' THEN printf('00000002-0000-4000-8000-%012x', status_ref_id)
                           ELSE printf('00000007-0000-4000-8000-%012x', status_ref_id)
                       END,
                       model_id, original_purchase_price_cents
                FROM trains_v16;

                INSERT INTO passenger_services(id, sequence, name)
                SELECT printf('00000006-0000-4000-8000-%012x', id), id, name
                FROM passenger_services_v16;

                INSERT INTO service_stops(service_id, sequence, station_id)
                SELECT printf('00000006-0000-4000-8000-%012x', service_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', station_id)
                FROM service_stops_v16;

                INSERT INTO service_lines(service_id, sequence, rail_line_id)
                SELECT printf('00000006-0000-4000-8000-%012x', service_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', rail_line_id)
                FROM service_lines_v16;

                INSERT INTO origin_destination_demand(
                    origin_station_id, destination_station_id, sequence, waiting_passengers,
                    passenger_arrival_rate_per_hour, fractional_passenger_seconds
                )
                SELECT printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       ROW_NUMBER() OVER (ORDER BY origin_station_id, destination_station_id) - 1,
                       waiting_passengers, passenger_arrival_rate_per_hour, fractional_passenger_seconds
                FROM origin_destination_demand_v16;

                INSERT INTO active_journeys(
                    id, sequence, service_id, train_id, origin_station_id, destination_station_id,
                    passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents,
                    infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index,
                    departed_at, arrives_at
                )
                SELECT printf('00000007-0000-4000-8000-%012x', id), id,
                       printf('00000006-0000-4000-8000-%012x', service_id),
                       printf('00000005-0000-4000-8000-%012x', train_id),
                       printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents,
                       infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index,
                       departed_at, arrives_at
                FROM active_journeys_v16;

                INSERT INTO journey_passenger_groups(
                    journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents
                )
                SELECT printf('00000007-0000-4000-8000-%012x', journey_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       passengers, fare_cents
                FROM journey_passenger_groups_v16;

                INSERT INTO journey_receipts(
                    journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents,
                    train_id, train_model_name, origin_station_id, destination_station_id,
                    passengers_carried, passenger_capacity, completed_at
                )
                SELECT printf('00000007-0000-4000-8000-%012x', journey_id), revenue_cents,
                       infrastructure_access_fee_cents, fuel_cost_cents,
                       CASE WHEN train_id IS NULL THEN NULL ELSE printf('00000005-0000-4000-8000-%012x', train_id) END,
                       train_model_name,
                       CASE WHEN origin_station_id IS NULL THEN NULL ELSE printf('00000002-0000-4000-8000-%012x', origin_station_id) END,
                       CASE WHEN destination_station_id IS NULL THEN NULL ELSE printf('00000002-0000-4000-8000-%012x', destination_station_id) END,
                       passengers_carried, passenger_capacity, completed_at
                FROM journey_receipts_v16;

                DROP TABLE journey_passenger_groups_v16;
                DROP TABLE active_journeys_v16;
                DROP TABLE origin_destination_demand_v16;
                DROP TABLE service_lines_v16;
                DROP TABLE service_stops_v16;
                DROP TABLE passenger_services_v16;
                DROP TABLE trains_v16;
                DROP TABLE company_v16;
                DROP TABLE infrastructure_project_planned_lines_v16;
                DROP TABLE infrastructure_project_planned_stations_v16;
                DROP TABLE infrastructure_project_rail_stations_v16;
                DROP TABLE infrastructure_project_rail_lines_v16;
                DROP TABLE infrastructure_projects_v16;
                DROP TABLE rail_lines_v16;
                DROP TABLE rail_stations_v16;
                DROP TABLE settlements_v16;
                DROP TABLE region_v16;
                DROP TABLE journey_receipts_v16;

                CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);
                CREATE INDEX IF NOT EXISTS idx_receipts_completed_at ON journey_receipts(completed_at);
                "#,
            )
            .map_err(|source| db_error("migrate entity IDs to UUID v4 in", path, source))?;

        connection
            .pragma_update(None, "user_version", 17_u32)
            .map_err(|source| db_error("write v17 schema version to", path, source))?;
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
                .map_err(|source| db_error("commit UUID migration for", path, source))?;
            let violation: Option<String> = connection
                .query_row(
                    "SELECT table FROM pragma_foreign_key_check LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("verify UUID migration for", path, source))?;
            if violation.is_some() {
                return Err(invalid_value(path, "foreign keys after UUID migration"));
            }
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

fn migrate_v17_to_v18(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v17 to v18 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let has_capacity_column: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('region')
                 WHERE name = 'rail_authority_construction_capacity'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect Rail Authority construction capacity in",
                    path,
                    source,
                )
            })?;
        if has_capacity_column == 0 {
            connection
                .execute(
                    "ALTER TABLE region
                     ADD COLUMN rail_authority_construction_capacity INTEGER NOT NULL DEFAULT 1
                     CHECK (rail_authority_construction_capacity > 0)",
                    [],
                )
                .map_err(|source| {
                    db_error("add Rail Authority construction capacity to", path, source)
                })?;
        }
        connection
            .pragma_update(None, "user_version", 18_u32)
            .map_err(|source| db_error("write v18 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v17 to v18 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v18_to_v19(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v18 to v19 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let project_table_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'infrastructure_projects'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Infrastructure Project table in", path, source))?;

        if project_table_exists != 0 {
            let has_estimated_cost: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('infrastructure_projects')
                     WHERE name = 'estimated_cost_cents'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| db_error("inspect project estimated cost in", path, source))?;
            if has_estimated_cost == 0 {
                connection
                    .execute(
                        "ALTER TABLE infrastructure_projects
                         ADD COLUMN estimated_cost_cents INTEGER NOT NULL DEFAULT 0
                         CHECK (estimated_cost_cents >= 0)",
                        [],
                    )
                    .map_err(|source| db_error("add project estimated cost to", path, source))?;
            }

            let has_authority_commitment: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('infrastructure_projects')
                     WHERE name = 'authority_committed_cents'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    db_error("inspect project Authority commitment in", path, source)
                })?;
            if has_authority_commitment == 0 {
                connection
                    .execute(
                        "ALTER TABLE infrastructure_projects
                         ADD COLUMN authority_committed_cents INTEGER NOT NULL DEFAULT 0
                         CHECK (authority_committed_cents >= 0)",
                        [],
                    )
                    .map_err(|source| {
                        db_error("add project Authority commitment to", path, source)
                    })?;
            }

            let planned_lines_exist: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'infrastructure_project_planned_lines'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| db_error("inspect planned Rail Line table in", path, source))?;
            if planned_lines_exist != 0 {
                connection
                    .execute_batch(
                        "UPDATE infrastructure_projects
                         SET estimated_cost_cents = COALESCE((
                             SELECT SUM(
                                 ((pl.distance_metres * CASE pl.construction_difficulty
                                     WHEN 'low' THEN 120000
                                     WHEN 'moderate' THEN 160000
                                     WHEN 'high' THEN 220000
                                     ELSE 160000
                                 END) + 999) / 1000
                             )
                             FROM infrastructure_project_planned_lines pl
                             WHERE pl.project_id = infrastructure_projects.id
                         ), 0)
                         WHERE kind = 'new_line' AND estimated_cost_cents = 0;",
                    )
                    .map_err(|source| {
                        db_error("backfill project estimated cost in", path, source)
                    })?;
            }
        }

        connection
            .pragma_update(None, "user_version", 19_u32)
            .map_err(|source| db_error("write v19 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v18 to v19 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v19_to_v20(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v19 to v20 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE settlements ADD COLUMN world_x INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE settlements ADD COLUMN world_y INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|source| db_error("add Settlement coordinates to", path, source))?;

        let world_seed_text: String = connection
            .query_row(
                "SELECT world_seed FROM game_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "read world seed for Settlement coordinate migration from",
                    path,
                    source,
                )
            })?;
        let world_seed =
            world_seed_text
                .parse::<u64>()
                .map_err(|_| SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::InvalidValue {
                        field: "World Seed",
                    }),
                })?;
        let settlement_count: usize = connection
            .query_row("SELECT COUNT(*) FROM settlements", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| {
                db_error(
                    "count Settlements for coordinate migration in",
                    path,
                    source,
                )
            })?
            .try_into()
            .map_err(|_| SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "Settlement Count",
                }),
            })?;
        let positions = settlement_positions_for_existing_region(world_seed, settlement_count);
        for (sequence, position) in positions.into_iter().enumerate() {
            connection
                .execute(
                    "UPDATE settlements SET world_x = ?1, world_y = ?2 WHERE sequence = ?3",
                    params![
                        position.x,
                        position.y,
                        i64::try_from(sequence).unwrap_or(i64::MAX)
                    ],
                )
                .map_err(|source| db_error("backfill Settlement coordinates in", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 20_u32)
            .map_err(|source| db_error("write v20 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v19 to v20 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v20_to_v21(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v20 to v21 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE infrastructure_projects
                 ADD COLUMN operator_contributed_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (operator_contributed_cents >= 0);",
            )
            .map_err(|source| {
                db_error("add operator infrastructure contributions to", path, source)
            })?;
        connection
            .pragma_update(None, "user_version", 21_u32)
            .map_err(|source| db_error("write v21 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v20 to v21 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v21_to_v22(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v21 to v22 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE infrastructure_projects
                 ADD COLUMN access_fee_credit_awarded_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (access_fee_credit_awarded_cents >= 0);
                 ALTER TABLE infrastructure_projects
                 ADD COLUMN access_fee_credit_remaining_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (access_fee_credit_remaining_cents >= 0);
                 UPDATE infrastructure_projects
                 SET access_fee_credit_awarded_cents = (operator_contributed_cents * 115) / 100,
                     access_fee_credit_remaining_cents = (operator_contributed_cents * 115) / 100
                 WHERE status = 'open' AND operator_contributed_cents > 0;",
            )
            .map_err(|source| db_error("add infrastructure access credits to", path, source))?;
        connection
            .pragma_update(None, "user_version", 22_u32)
            .map_err(|source| db_error("write v22 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v21 to v22 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

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
    let envelope: LegacySaveEnvelope<LegacyGameStateV1> =
        ron::from_str(source).map_err(|error| SaveCodecError::LegacyDecode(error.to_string()))?;
    if envelope.version != LEGACY_RON_VERSION {
        return Err(SaveCodecError::UnsupportedVersion {
            found: envelope.version,
        });
    }

    let legacy = envelope.state;
    let mut region = legacy.region;
    region.railway_registration =
        railway_registration_for_existing_region(&region.name, legacy.world_seed);
    let mut next_evn_unit_by_model: std::collections::BTreeMap<TrainModelId, u16> =
        std::collections::BTreeMap::new();
    let trains = legacy
        .player_company
        .fleet
        .trains
        .into_iter()
        .map(|train| {
            let passenger_capacity = i64::from(train.passenger_capacity.passengers());
            let speed = i64::try_from(train.speed.metres_per_second()).map_err(|_| {
                SaveCodecError::InvalidValue {
                    field: "legacy Train speed",
                }
            })?;
            let fuel_rate = i64::try_from(train.fuel_cost_per_kilometre.cents_per_kilometre())
                .map_err(|_| SaveCodecError::InvalidValue {
                    field: "legacy Train fuel rate",
                })?;
            let model = catalogue_model_for_legacy_signature(
                &train.model_name,
                passenger_capacity,
                speed,
                fuel_rate,
            )
            .ok_or_else(|| SaveCodecError::TrainModelNotFound {
                model_name: train.model_name.clone(),
            })?;
            let unit_number = next_evn_unit_by_model
                .entry(model.id().clone())
                .or_insert(1);
            let evn = EuropeanVehicleNumber::generate(
                model.evn_type_code(),
                region.railway_registration.numeric_code,
                model.evn_series_code(),
                *unit_number,
            )
            .map_err(|_| SaveCodecError::InvalidValue {
                field: "European Vehicle Number",
            })?;
            let next_unit_number =
                (*unit_number)
                    .checked_add(1)
                    .ok_or(SaveCodecError::InvalidValue {
                        field: "EVN unit number",
                    })?;
            *unit_number = next_unit_number;
            Ok(Train {
                id: train.id,
                evn,
                nickname: None,
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
            if journey.origin_station_id
                == service
                    .destination_station_id()
                    .unwrap_or(journey.origin_station_id)
                && journey.destination_station_id
                    == service
                        .origin_station_id()
                        .unwrap_or(journey.destination_station_id)
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
            fleet: Fleet {
                next_train_display_number: trains
                    .iter()
                    .map(|train| train.id.get())
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1),
                trains,
                next_evn_unit_by_model,
            },
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
        let legacy_trains = state
            .player_company
            .fleet
            .trains
            .iter()
            .map(|train| {
                let model = model_for_train(train).unwrap();
                LegacyTrainV1 {
                    id: train.id,
                    status: train.status.clone(),
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

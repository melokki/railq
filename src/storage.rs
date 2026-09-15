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
    catalog::{TrainModel, model_for_train, train_catalogue},
    model::{
        CalculationError, ConstructionDifficulty, DemandRules, DistanceMetres, DurationSeconds,
        Electrification, EuropeanVehicleNumber, Financials, Fleet, GameRules, GameState,
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
pub const SAVE_VERSION: u32 = 25;

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
    rail_authority_name TEXT NOT NULL,
    rail_authority_construction_capacity INTEGER NOT NULL DEFAULT 1 CHECK (rail_authority_construction_capacity > 0)
);
CREATE TABLE IF NOT EXISTS rail_authority_finances (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
    maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
    committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
    carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
    regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
    infrastructure_access_fee_revenue_cents INTEGER NOT NULL CHECK (infrastructure_access_fee_revenue_cents >= 0),
    next_fiscal_period_at INTEGER
);
CREATE TABLE IF NOT EXISTS settlements (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    population INTEGER NOT NULL,
    world_x INTEGER NOT NULL,
    world_y INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rail_stations (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    settlement_id TEXT NOT NULL REFERENCES settlements(id)
);
CREATE TABLE IF NOT EXISTS rail_lines (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    first_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    second_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    distance_metres INTEGER NOT NULL,
    speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
    track_count INTEGER NOT NULL CHECK (track_count > 0),
    electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
    construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high'))
);
CREATE TABLE IF NOT EXISTS infrastructure_projects (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('new_line', 'speed_upgrade', 'double_tracking', 'electrification', 'renewal', 'station_upgrade')),
    status TEXT NOT NULL CHECK (status IN ('requested', 'under_review', 'proposed', 'approved', 'deferred', 'funding', 'scheduled', 'construction', 'open', 'cancelled')),
    estimated_cost_cents INTEGER NOT NULL DEFAULT 0 CHECK (estimated_cost_cents >= 0),
    authority_committed_cents INTEGER NOT NULL DEFAULT 0 CHECK (authority_committed_cents >= 0),
    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),
    access_fee_credit_awarded_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_awarded_cents >= 0),
    access_fee_credit_remaining_cents INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_credit_remaining_cents >= 0),
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
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_lines (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_stations (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_stations (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id TEXT NOT NULL UNIQUE,
    settlement_id TEXT NOT NULL REFERENCES settlements(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_lines (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    line_id TEXT NOT NULL UNIQUE,
    first_station_id TEXT NOT NULL,
    second_station_id TEXT NOT NULL,
    distance_metres INTEGER NOT NULL CHECK (distance_metres > 0),
    speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
    track_count INTEGER NOT NULL CHECK (track_count > 0),
    electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
    construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high')),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS company (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    vkm TEXT NOT NULL CHECK (length(vkm) BETWEEN 2 AND 5) CHECK (vkm NOT GLOB '*[^A-Z]*'),
    funds_cents INTEGER NOT NULL,
    next_train_display_number INTEGER NOT NULL CHECK (next_train_display_number > 0)
);
CREATE TABLE IF NOT EXISTS trains (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    evn TEXT NOT NULL UNIQUE CHECK (length(evn) = 12) CHECK (evn NOT GLOB '*[^0-9]*'),
    nickname TEXT CHECK (nickname IS NULL OR length(trim(nickname)) BETWEEN 1 AND 32),
    status_kind TEXT NOT NULL CHECK (status_kind IN ('ready', 'travelling')),
    status_ref_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    original_purchase_price_cents INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS train_model_sequences (
    model_id TEXT PRIMARY KEY,
    next_unit_number INTEGER NOT NULL CHECK (next_unit_number BETWEEN 1 AND 1000)
);
CREATE TABLE IF NOT EXISTS passenger_services (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS service_stops (
    service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id TEXT NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS service_lines (
    service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS origin_destination_demand (
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    sequence INTEGER NOT NULL UNIQUE,
    waiting_passengers INTEGER NOT NULL,
    market_maturity_basis_points INTEGER NOT NULL DEFAULT 10000
        CHECK (market_maturity_basis_points BETWEEN 0 AND 10000),
    passenger_arrival_rate_per_hour INTEGER NOT NULL,
    fractional_passenger_seconds INTEGER NOT NULL,
    PRIMARY KEY (origin_station_id, destination_station_id)
);
CREATE TABLE IF NOT EXISTS active_journeys (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    service_id TEXT NOT NULL REFERENCES passenger_services(id),
    train_id TEXT NOT NULL REFERENCES trains(id),
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
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
    journey_id TEXT NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
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
    journey_id TEXT PRIMARY KEY,
    revenue_cents INTEGER NOT NULL,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    train_id TEXT,
    train_model_name TEXT,
    origin_station_id TEXT,
    destination_station_id TEXT,
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
            connection
                .execute_batch(SCHEMA)
                .map_err(|source| SaveSlotError::Database {
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
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        2 => {
            migrate_v2_to_v3(connection, path)?;
            migrate_v3_to_v4(connection, path)?;
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        3 => {
            migrate_v3_to_v4(connection, path)?;
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        4 => {
            migrate_v4_to_v5(connection, path)?;
            migrate_v5_to_v6(connection, path)?;
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        5 => {
            migrate_v5_to_v6(connection, path)?;
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        6 => {
            migrate_v6_to_v7(connection, path)?;
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        7 => {
            migrate_v7_to_v8(connection, path)?;
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        8 => {
            migrate_v8_to_v9(connection, path)?;
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        9 => {
            migrate_v9_to_v10(connection, path)?;
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        10 => {
            migrate_v10_to_v11(connection, path)?;
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        11 => {
            migrate_v11_to_v12(connection, path)?;
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        12 => {
            migrate_v12_to_v13(connection, path)?;
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        13 => {
            migrate_v13_to_v14(connection, path)?;
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        14 => {
            migrate_v14_to_v15(connection, path)?;
            migrate_v15_to_v16(connection, path)?;
        }
        15 => migrate_v15_to_v16(connection, path)?,
        16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 => {}
        SAVE_VERSION => {
            connection
                .execute_batch(SCHEMA)
                .map_err(|source| SaveSlotError::Database {
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

    let mut current_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| db_error("read migrated schema version from", path, source))?;
    if current_version == 16 {
        migrate_v16_to_v17(connection, path)?;
        current_version = 17;
    }
    if current_version == 17 {
        migrate_v17_to_v18(connection, path)?;
        current_version = 18;
    }
    if current_version == 18 {
        migrate_v18_to_v19(connection, path)?;
        current_version = 19;
    }
    if current_version == 19 {
        migrate_v19_to_v20(connection, path)?;
        current_version = 20;
    }
    if current_version == 20 {
        migrate_v20_to_v21(connection, path)?;
        current_version = 21;
    }
    if current_version == 21 {
        migrate_v21_to_v22(connection, path)?;
        current_version = 22;
    }
    if current_version == 22 {
        migrate_v22_to_v23(connection, path)?;
        current_version = 23;
    }
    if current_version == 23 {
        migrate_v23_to_v24(connection, path)?;
        current_version = 24;
    }
    if current_version == 24 {
        migrate_v24_to_v25(connection, path)?;
    }
    Ok(())
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
    transaction
        .execute_batch(
            "DELETE FROM journey_passenger_groups;
         DELETE FROM active_journeys;
         DELETE FROM origin_destination_demand;
         DELETE FROM service_lines;
         DELETE FROM service_stops;
         DELETE FROM passenger_services;
         DELETE FROM trains;
         DELETE FROM train_model_sequences;
         DELETE FROM financials;
         DELETE FROM game_rules;
         DELETE FROM company;
         DELETE FROM infrastructure_project_planned_lines;
         DELETE FROM infrastructure_project_planned_stations;
         DELETE FROM infrastructure_project_rail_stations;
         DELETE FROM infrastructure_project_rail_lines;
         DELETE FROM infrastructure_projects;
         DELETE FROM rail_authority_finances;
         DELETE FROM rail_lines;
         DELETE FROM rail_stations;
         DELETE FROM settlements;
         DELETE FROM region;
         DELETE FROM game_meta;",
        )
        .map_err(|source| SaveSlotError::Database {
            action: "clear previous state from",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn insert_state(
    transaction: &Transaction<'_>,
    state: &GameState,
    path: &Path,
) -> Result<(), SaveSlotError> {
    let db = |value: u64, field| to_db_u64(value, field, path);

    transaction
        .execute(
            "INSERT INTO game_meta(singleton, world_seed, last_processed_at) VALUES(1, ?1, ?2)",
            params![
                state.world_seed.to_string(),
                state.last_processed_at.unix_seconds()
            ],
        )
        .map_err(|source| db_error("write game metadata to", path, source))?;
    transaction
        .execute(
            "INSERT INTO region(
             singleton, name, registration_code, registration_mark, population,
             rail_authority_name, rail_authority_construction_capacity
         ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &state.region.name,
                i64::from(state.region.railway_registration.numeric_code),
                &state.region.railway_registration.mark,
                db(state.region.population, "Region Population")?,
                &state.region.rail_authority.name,
                i64::from(state.region.rail_authority.construction_capacity),
            ],
        )
        .map_err(|source| db_error("write Region to", path, source))?;

    let authority_finances = &state.region.rail_authority.finances;
    transaction
        .execute(
            "INSERT INTO rail_authority_finances(
                 singleton, treasury_cents, maintenance_reserve_cents,
                 committed_investment_cents, carried_over_funds_cents,
                 regional_public_allocation_cents, infrastructure_access_fee_revenue_cents,
                 next_fiscal_period_at
             ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                authority_finances.treasury.cents(),
                authority_finances.maintenance_reserve.cents(),
                authority_finances.committed_investment.cents(),
                authority_finances.carried_over_funds.cents(),
                authority_finances.regional_public_allocation.cents(),
                authority_finances.infrastructure_access_fee_revenue.cents(),
                authority_finances
                    .next_fiscal_period_at
                    .map(UtcSeconds::unix_seconds),
            ],
        )
        .map_err(|source| db_error("write Rail Authority finances to", path, source))?;

    for (sequence, settlement) in state.region.settlements.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO settlements(id, sequence, name, population, world_x, world_y) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    settlement.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &settlement.name,
                    db(settlement.population, "Settlement Population")?,
                    settlement.position.x,
                    settlement.position.y
                ],
            )
            .map_err(|source| db_error("write Settlements to", path, source))?;
    }
    let network = &state.region.rail_authority.rail_network;
    for (sequence, station) in network.rail_stations.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO rail_stations(id, sequence, settlement_id) VALUES(?1, ?2, ?3)",
                params![
                    station.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    station.settlement_id.to_string()
                ],
            )
            .map_err(|source| db_error("write Rail Stations to", path, source))?;
    }
    for (sequence, line) in network.rail_lines.iter().enumerate() {
        let electrification = match line.electrification {
            Electrification::None => "none",
            Electrification::Electric => "electric",
        };
        let construction_difficulty = match line.construction_difficulty {
            ConstructionDifficulty::Low => "low",
            ConstructionDifficulty::Moderate => "moderate",
            ConstructionDifficulty::High => "high",
        };
        transaction
            .execute(
                "INSERT INTO rail_lines(
                 id, sequence, first_station_id, second_station_id, distance_metres,
                 speed_limit_kmh, track_count, electrification, construction_difficulty
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    line.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    line.first_station_id.to_string(),
                    line.second_station_id.to_string(),
                    db(line.distance.metres(), "Rail Line distance")?,
                    i64::from(line.speed_limit.kilometres_per_hour()),
                    i64::from(line.track_count.tracks()),
                    electrification,
                    construction_difficulty,
                ],
            )
            .map_err(|source| db_error("write Rail Lines to", path, source))?;
    }

    for (sequence, project) in state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .enumerate()
    {
        insert_infrastructure_project(transaction, sequence, project, path)?;
    }

    transaction
        .execute(
            "INSERT INTO company(singleton, name, vkm, funds_cents, next_train_display_number)
         VALUES(1, ?1, ?2, ?3, ?4)",
            params![
                &state.player_company.name,
                state.player_company.vehicle_keeper_mark.as_str(),
                state.player_company.funds.cents(),
                to_db_u64(
                    state.player_company.fleet.next_train_display_number,
                    "next Train display number",
                    path,
                )?,
            ],
        )
        .map_err(|source| db_error("write Player Company to", path, source))?;

    for (model_id, next_unit_number) in &state.player_company.fleet.next_evn_unit_by_model {
        transaction
            .execute(
                "INSERT INTO train_model_sequences(model_id, next_unit_number) VALUES(?1, ?2)",
                params![model_id.as_str(), i64::from(*next_unit_number)],
            )
            .map_err(|source| db_error("write EVN model sequences to", path, source))?;
    }

    for (sequence, train) in state.player_company.fleet.trains.iter().enumerate() {
        let (status_kind, status_ref_id) = match train.status {
            TrainStatus::Ready { at } => ("ready", at.to_string()),
            TrainStatus::Travelling { journey_id } => ("travelling", journey_id.to_string()),
        };
        transaction.execute(
            "INSERT INTO trains(id, sequence, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                train.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), train.evn.as_str(),
                train.nickname.as_ref().map(TrainNickname::as_str), status_kind,
                status_ref_id, train.model_id.as_str(),
                train.original_purchase_price.cents()
            ],
        ).map_err(|source| db_error("write Trains to", path, source))?;
    }

    for (sequence, service) in state.player_company.passenger_services.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO passenger_services(id, sequence, name) VALUES(?1, ?2, ?3)",
                params![
                    service.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &service.name
                ],
            )
            .map_err(|source| db_error("write Passenger Services to", path, source))?;
        for (sequence, station_id) in service.stop_station_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_stops(service_id, sequence, station_id) VALUES(?1, ?2, ?3)",
                params![service.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), station_id.to_string()],
            ).map_err(|source| db_error("write Passenger Service stops to", path, source))?;
        }
        for (sequence, line_id) in service.rail_line_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_lines(service_id, sequence, rail_line_id) VALUES(?1, ?2, ?3)",
                params![service.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), line_id.to_string()],
            ).map_err(|source| db_error("write Passenger Service paths to", path, source))?;
        }
    }

    for (sequence, demand) in state.origin_destination_demand.iter().enumerate() {
        transaction.execute(
            "INSERT INTO origin_destination_demand(origin_station_id, destination_station_id, sequence, waiting_passengers, market_maturity_basis_points, passenger_arrival_rate_per_hour, fractional_passenger_seconds)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                demand.origin_station_id.to_string(), demand.destination_station_id.to_string(),
                i64::try_from(sequence).unwrap_or(i64::MAX),
                i64::from(demand.waiting_passengers), i64::from(demand.market_maturity.basis_points()),
                i64::from(demand.passenger_arrival_rate_per_hour.passengers_per_hour()),
                db(demand.fractional_passenger_seconds, "Demand fractional passenger seconds")?
            ],
        ).map_err(|source| db_error("write Passenger Demand to", path, source))?;
    }

    for (sequence, journey) in state.active_journeys.iter().enumerate() {
        transaction.execute(
            "INSERT INTO active_journeys(id, sequence, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                journey.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), journey.service_id.to_string(), journey.train_id.to_string(),
                journey.origin_station_id.to_string(), journey.destination_station_id.to_string(), i64::from(journey.passengers_carried),
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
                    journey.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    group.origin_station_id.to_string(),
                    group.destination_station_id.to_string(),
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
                receipt.journey_id.to_string(), receipt.revenue.cents(), receipt.infrastructure_access_fee.cents(), receipt.fuel_cost.cents(),
                receipt.train_id.map(|id| id.to_string()), receipt.train_model_name.as_deref(),
                receipt.origin_station_id.map(|id| id.to_string()),
                receipt.destination_station_id.map(|id| id.to_string()),
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

fn insert_infrastructure_project(
    transaction: &Transaction<'_>,
    sequence: usize,
    project: &InfrastructureProject,
    path: &Path,
) -> Result<(), SaveSlotError> {
    let (kind, target_speed_limit_kmh, target_track_count) = match &project.kind {
        InfrastructureProjectKind::NewLine { .. } => ("new_line", None, None),
        InfrastructureProjectKind::SpeedUpgrade {
            target_speed_limit, ..
        } => (
            "speed_upgrade",
            Some(i64::from(target_speed_limit.kilometres_per_hour())),
            None,
        ),
        InfrastructureProjectKind::DoubleTracking {
            target_track_count, ..
        } => (
            "double_tracking",
            None,
            Some(i64::from(target_track_count.tracks())),
        ),
        InfrastructureProjectKind::Electrification { .. } => ("electrification", None, None),
        InfrastructureProjectKind::Renewal { .. } => ("renewal", None, None),
        InfrastructureProjectKind::StationUpgrade { .. } => ("station_upgrade", None, None),
    };
    let status = match project.status {
        InfrastructureProjectStatus::Requested => "requested",
        InfrastructureProjectStatus::UnderReview => "under_review",
        InfrastructureProjectStatus::Proposed => "proposed",
        InfrastructureProjectStatus::Approved => "approved",
        InfrastructureProjectStatus::Deferred => "deferred",
        InfrastructureProjectStatus::Funding => "funding",
        InfrastructureProjectStatus::Scheduled => "scheduled",
        InfrastructureProjectStatus::Construction => "construction",
        InfrastructureProjectStatus::Open => "open",
        InfrastructureProjectStatus::Cancelled => "cancelled",
    };
    let timeline = &project.timeline;
    transaction
        .execute(
            "INSERT INTO infrastructure_projects(
                 id, sequence, kind, status, estimated_cost_cents, authority_committed_cents,
                 operator_contributed_cents, access_fee_credit_awarded_cents, access_fee_credit_remaining_cents,
                 requested_at, review_started_at, proposed_at, approved_at, funding_completed_at, scheduled_start_at,
                 construction_started_at, planned_completion_at, completed_at, deferred_at, cancelled_at,
                 target_speed_limit_kmh, target_track_count
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                project.id.to_string(),
                i64::try_from(sequence).unwrap_or(i64::MAX),
                kind,
                status,
                project.funding.estimated_cost.cents(),
                project.funding.authority_committed.cents(),
                project.funding.operator_contributed.cents(),
                project.funding.access_fee_credit_awarded.cents(),
                project.funding.access_fee_credit_remaining.cents(),
                timeline.requested_at.unix_seconds(),
                timeline.review_started_at.map(UtcSeconds::unix_seconds),
                timeline.proposed_at.map(UtcSeconds::unix_seconds),
                timeline.approved_at.map(UtcSeconds::unix_seconds),
                timeline.funding_completed_at.map(UtcSeconds::unix_seconds),
                timeline.scheduled_start_at.map(UtcSeconds::unix_seconds),
                timeline.construction_started_at.map(UtcSeconds::unix_seconds),
                timeline.planned_completion_at.map(UtcSeconds::unix_seconds),
                timeline.completed_at.map(UtcSeconds::unix_seconds),
                timeline.deferred_at.map(UtcSeconds::unix_seconds),
                timeline.cancelled_at.map(UtcSeconds::unix_seconds),
                target_speed_limit_kmh,
                target_track_count,
            ],
        )
        .map_err(|source| db_error("write infrastructure projects to", path, source))?;

    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            for (sequence, station) in planned_stations.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_planned_stations(
                             project_id, sequence, station_id, settlement_id
                         ) VALUES(?1, ?2, ?3, ?4)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            station.id.to_string(),
                            station.settlement_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write planned Rail Stations to", path, source))?;
            }
            for (sequence, line) in planned_lines.iter().enumerate() {
                let electrification = match line.electrification {
                    Electrification::None => "none",
                    Electrification::Electric => "electric",
                };
                let construction_difficulty = match line.construction_difficulty {
                    ConstructionDifficulty::Low => "low",
                    ConstructionDifficulty::Moderate => "moderate",
                    ConstructionDifficulty::High => "high",
                };
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_planned_lines(
                             project_id, sequence, line_id, first_station_id, second_station_id,
                             distance_metres, speed_limit_kmh, track_count, electrification,
                             construction_difficulty
                         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            line.id.to_string(),
                            line.first_station_id.to_string(),
                            line.second_station_id.to_string(),
                            to_db_u64(line.distance.metres(), "planned Rail Line distance", path)?,
                            i64::from(line.speed_limit.kilometres_per_hour()),
                            i64::from(line.track_count.tracks()),
                            electrification,
                            construction_difficulty,
                        ],
                    )
                    .map_err(|source| db_error("write planned Rail Lines to", path, source))?;
            }
        }
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
        | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
        | InfrastructureProjectKind::Electrification { rail_line_ids }
        | InfrastructureProjectKind::Renewal { rail_line_ids } => {
            for (sequence, rail_line_id) in rail_line_ids.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_rail_lines(project_id, sequence, rail_line_id)
                         VALUES(?1, ?2, ?3)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            rail_line_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write infrastructure project Rail Lines to", path, source))?;
            }
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            for (sequence, rail_station_id) in rail_station_ids.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_rail_stations(project_id, sequence, rail_station_id)
                         VALUES(?1, ?2, ?3)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            rail_station_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write infrastructure project Rail Stations to", path, source))?;
            }
        }
    }

    Ok(())
}

fn load_infrastructure_projects(
    connection: &Connection,
    path: &Path,
) -> Result<Vec<InfrastructureProject>, SaveSlotError> {
    #[derive(Debug)]
    struct PersistedProject {
        id: InfrastructureProjectId,
        kind: String,
        status: String,
        funding: InfrastructureProjectFunding,
        timeline: InfrastructureProjectTimeline,
        target_speed_limit_kmh: Option<i64>,
        target_track_count: Option<i64>,
    }

    let rows = query_all(
        connection,
        "SELECT id, kind, status, estimated_cost_cents, authority_committed_cents,
                operator_contributed_cents, access_fee_credit_awarded_cents, access_fee_credit_remaining_cents,
                requested_at, review_started_at, proposed_at, approved_at, funding_completed_at, scheduled_start_at,
                construction_started_at, planned_completion_at, completed_at, deferred_at, cancelled_at,
                target_speed_limit_kmh, target_track_count
         FROM infrastructure_projects ORDER BY sequence",
        path,
        |row| {
            let timestamp = |index: usize| -> rusqlite::Result<Option<UtcSeconds>> {
                Ok(row
                    .get::<_, Option<i64>>(index)?
                    .map(UtcSeconds::from_unix_seconds))
            };
            Ok(PersistedProject {
                id: row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                kind: row.get(1)?,
                status: row.get(2)?,
                funding: InfrastructureProjectFunding {
                    estimated_cost: Money::from_cents(row.get(3)?),
                    authority_committed: Money::from_cents(row.get(4)?),
                    operator_contributed: Money::from_cents(row.get(5)?),
                    access_fee_credit_awarded: Money::from_cents(row.get(6)?),
                    access_fee_credit_remaining: Money::from_cents(row.get(7)?),
                },
                timeline: InfrastructureProjectTimeline {
                    requested_at: UtcSeconds::from_unix_seconds(row.get(8)?),
                    review_started_at: timestamp(9)?,
                    proposed_at: timestamp(10)?,
                    approved_at: timestamp(11)?,
                    funding_completed_at: timestamp(12)?,
                    scheduled_start_at: timestamp(13)?,
                    construction_started_at: timestamp(14)?,
                    planned_completion_at: timestamp(15)?,
                    completed_at: timestamp(16)?,
                    deferred_at: timestamp(17)?,
                    cancelled_at: timestamp(18)?,
                },
                target_speed_limit_kmh: row.get(19)?,
                target_track_count: row.get(20)?,
            })
        },
    )?;

    let mut line_targets: HashMap<InfrastructureProjectId, Vec<RailLineId>> = HashMap::new();
    for (project_id, rail_line_id) in query_all(
        connection,
        "SELECT project_id, rail_line_id
         FROM infrastructure_project_rail_lines ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                row_domain_id(row, 1, "Rail Line ID", RailLineId::parse)?,
            ))
        },
    )? {
        line_targets
            .entry(project_id)
            .or_default()
            .push(rail_line_id);
    }

    let mut station_targets: HashMap<InfrastructureProjectId, Vec<RailStationId>> = HashMap::new();
    for (project_id, rail_station_id) in query_all(
        connection,
        "SELECT project_id, rail_station_id
         FROM infrastructure_project_rail_stations ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                row_domain_id(row, 1, "Rail Station ID", RailStationId::parse)?,
            ))
        },
    )? {
        station_targets
            .entry(project_id)
            .or_default()
            .push(rail_station_id);
    }

    let mut planned_stations: HashMap<InfrastructureProjectId, Vec<PlannedRailStation>> =
        HashMap::new();
    for (project_id, station) in query_all(
        connection,
        "SELECT project_id, station_id, settlement_id
         FROM infrastructure_project_planned_stations ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                PlannedRailStation {
                    id: row_domain_id(row, 1, "planned Rail Station ID", RailStationId::parse)?,
                    settlement_id: row_domain_id(row, 2, "Settlement ID", SettlementId::parse)?,
                },
            ))
        },
    )? {
        planned_stations
            .entry(project_id)
            .or_default()
            .push(station);
    }

    let mut planned_lines: HashMap<InfrastructureProjectId, Vec<PlannedRailLine>> = HashMap::new();
    for (project_id, line) in query_all(
        connection,
        "SELECT project_id, line_id, first_station_id, second_station_id, distance_metres,
                speed_limit_kmh, track_count, electrification, construction_difficulty
         FROM infrastructure_project_planned_lines ORDER BY project_id, sequence",
        path,
        |row| {
            let electrification = match row.get::<_, String>(7)?.as_str() {
                "none" => Electrification::None,
                "electric" => Electrification::Electric,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let construction_difficulty = match row.get::<_, String>(8)?.as_str() {
                "low" => ConstructionDifficulty::Low,
                "moderate" => ConstructionDifficulty::Moderate,
                "high" => ConstructionDifficulty::High,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                PlannedRailLine {
                    id: row_domain_id(row, 1, "planned Rail Line ID", RailLineId::parse)?,
                    first_station_id: row_domain_id(
                        row,
                        2,
                        "planned Rail Line endpoint",
                        RailStationId::parse,
                    )?,
                    second_station_id: row_domain_id(
                        row,
                        3,
                        "planned Rail Line endpoint",
                        RailStationId::parse,
                    )?,
                    distance: DistanceMetres::new(row.get(4)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    speed_limit: SpeedKilometresPerHour::new(row.get(5)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    track_count: TrackCount::new(row.get(6)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    electrification,
                    construction_difficulty,
                },
            ))
        },
    )? {
        planned_lines.entry(project_id).or_default().push(line);
    }

    let mut projects = Vec::with_capacity(rows.len());
    for row in rows {
        let status = match row.status.as_str() {
            "requested" => InfrastructureProjectStatus::Requested,
            "under_review" => InfrastructureProjectStatus::UnderReview,
            "proposed" => InfrastructureProjectStatus::Proposed,
            "approved" => InfrastructureProjectStatus::Approved,
            "deferred" => InfrastructureProjectStatus::Deferred,
            "funding" => InfrastructureProjectStatus::Funding,
            "scheduled" => InfrastructureProjectStatus::Scheduled,
            "construction" => InfrastructureProjectStatus::Construction,
            "open" => InfrastructureProjectStatus::Open,
            "cancelled" => InfrastructureProjectStatus::Cancelled,
            _ => return Err(invalid_value(path, "Infrastructure Project status")),
        };
        let kind = match row.kind.as_str() {
            "new_line" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || line_targets.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "New Line project payload"));
                }
                InfrastructureProjectKind::NewLine {
                    planned_stations: planned_stations.remove(&row.id).unwrap_or_default(),
                    planned_lines: planned_lines.remove(&row.id).unwrap_or_default(),
                }
            }
            "speed_upgrade" => {
                if row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Speed Upgrade project payload"));
                }
                let target_speed_limit = SpeedKilometresPerHour::new(
                    row.target_speed_limit_kmh
                        .ok_or_else(|| invalid_value(path, "Speed Upgrade target speed"))?,
                )
                .map_err(|_| invalid_value(path, "Speed Upgrade target speed"))?;
                InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: line_targets.remove(&row.id).unwrap_or_default(),
                    target_speed_limit,
                }
            }
            "double_tracking" => {
                if row.target_speed_limit_kmh.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Double Tracking project payload"));
                }
                let target_track_count = TrackCount::new(
                    row.target_track_count
                        .ok_or_else(|| invalid_value(path, "Double Tracking target tracks"))?,
                )
                .map_err(|_| invalid_value(path, "Double Tracking target tracks"))?;
                InfrastructureProjectKind::DoubleTracking {
                    rail_line_ids: line_targets.remove(&row.id).unwrap_or_default(),
                    target_track_count,
                }
            }
            "electrification" | "renewal" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Rail Line project payload"));
                }
                let rail_line_ids = line_targets.remove(&row.id).unwrap_or_default();
                if row.kind == "electrification" {
                    InfrastructureProjectKind::Electrification { rail_line_ids }
                } else {
                    InfrastructureProjectKind::Renewal { rail_line_ids }
                }
            }
            "station_upgrade" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || line_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Station Upgrade project payload"));
                }
                InfrastructureProjectKind::StationUpgrade {
                    rail_station_ids: station_targets.remove(&row.id).unwrap_or_default(),
                }
            }
            _ => return Err(invalid_value(path, "Infrastructure Project kind")),
        };
        projects.push(InfrastructureProject {
            id: row.id,
            kind,
            status,
            timeline: row.timeline,
            funding: row.funding,
        });
    }

    if !line_targets.is_empty()
        || !station_targets.is_empty()
        || !planned_stations.is_empty()
        || !planned_lines.is_empty()
    {
        return Err(invalid_value(path, "orphan infrastructure project payload"));
    }

    Ok(projects)
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
    let world_seed = world_seed_text
        .parse::<u64>()
        .map_err(|_| invalid_value(path, "world seed"))?;

    let (
        region_name,
        registration_code,
        registration_mark,
        region_population,
        authority_name,
        authority_construction_capacity,
    ): (String, i64, String, i64, String, i64) = connection
        .query_row(
            "SELECT name, registration_code, registration_mark, population, rail_authority_name,
                    rail_authority_construction_capacity
             FROM region WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|source| db_error("read Region from", path, source))?;

    let (
        authority_treasury,
        maintenance_reserve,
        committed_investment,
        carried_over_funds,
        regional_public_allocation,
        infrastructure_access_fee_revenue,
        next_fiscal_period_at,
    ): (i64, i64, i64, i64, i64, i64, Option<i64>) = connection
        .query_row(
            "SELECT treasury_cents, maintenance_reserve_cents, committed_investment_cents,
                    carried_over_funds_cents, regional_public_allocation_cents,
                    infrastructure_access_fee_revenue_cents, next_fiscal_period_at
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(|source| db_error("read Rail Authority finances from", path, source))?;
    let rail_authority_finances = RailAuthorityFinances {
        treasury: Money::from_cents(authority_treasury),
        maintenance_reserve: Money::from_cents(maintenance_reserve),
        committed_investment: Money::from_cents(committed_investment),
        carried_over_funds: Money::from_cents(carried_over_funds),
        regional_public_allocation: Money::from_cents(regional_public_allocation),
        infrastructure_access_fee_revenue: Money::from_cents(infrastructure_access_fee_revenue),
        next_fiscal_period_at: next_fiscal_period_at.map(UtcSeconds::from_unix_seconds),
    };

    let settlements = query_all(
        connection,
        "SELECT id, name, population, world_x, world_y FROM settlements ORDER BY sequence",
        path,
        |row| {
            Ok(Settlement {
                id: row_domain_id(row, 0, "Settlement ID", SettlementId::parse)?,
                name: row.get(1)?,
                population: row_u64(row, 2, "Settlement Population")?,
                position: WorldPosition::new(row.get(3)?, row.get(4)?),
            })
        },
    )?;
    let rail_stations = query_all(
        connection,
        "SELECT id, settlement_id FROM rail_stations ORDER BY sequence",
        path,
        |row| {
            Ok(RailStation {
                id: row_domain_id(row, 0, "Rail Station ID", RailStationId::parse)?,
                settlement_id: row_domain_id(row, 1, "Settlement ID", SettlementId::parse)?,
            })
        },
    )?;
    let rail_lines = query_all(
        connection,
        "SELECT id, first_station_id, second_station_id, distance_metres,
                speed_limit_kmh, track_count, electrification, construction_difficulty
         FROM rail_lines ORDER BY sequence",
        path,
        |row| {
            let electrification = match row.get::<_, String>(6)?.as_str() {
                "none" => Electrification::None,
                "electric" => Electrification::Electric,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let construction_difficulty = match row.get::<_, String>(7)?.as_str() {
                "low" => ConstructionDifficulty::Low,
                "moderate" => ConstructionDifficulty::Moderate,
                "high" => ConstructionDifficulty::High,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(RailLine {
                id: row_domain_id(row, 0, "Rail Line ID", RailLineId::parse)?,
                first_station_id: row_domain_id(row, 1, "Rail Station ID", RailStationId::parse)?,
                second_station_id: row_domain_id(row, 2, "Rail Station ID", RailStationId::parse)?,
                distance: DistanceMetres::new(row.get(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                speed_limit: SpeedKilometresPerHour::new(row.get(4)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                track_count: TrackCount::new(row.get(5)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                electrification,
                construction_difficulty,
            })
        },
    )?;

    let infrastructure_projects = load_infrastructure_projects(connection, path)?;

    let (company_name, company_vkm, company_funds, next_train_display_number):
        (String, String, i64, i64) = connection
        .query_row(
            "SELECT name, vkm, funds_cents, next_train_display_number FROM company WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
            .map_err(|source| db_error("read Player Company from", path, source))?;
    let company_vkm = VehicleKeeperMark::parse(&company_vkm)
        .map_err(|_| invalid_value(path, "Player Company VKM"))?;

    let next_evn_unit_by_model = query_all(
        connection,
        "SELECT model_id, next_unit_number FROM train_model_sequences ORDER BY model_id",
        path,
        |row| {
            let model_id = TrainModelId::new(row.get::<_, String>(0)?);
            let raw: i64 = row.get(1)?;
            let next_unit_number = u16::try_from(raw).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok((model_id, next_unit_number))
        },
    )?
    .into_iter()
    .collect();

    let trains = query_all(
        connection,
        "SELECT id, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents FROM trains ORDER BY sequence",
        path,
        |row| {
            let evn_text: String = row.get(1)?;
            let evn = EuropeanVehicleNumber::parse(&evn_text)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let nickname = row
                .get::<_, Option<String>>(2)?
                .map(|value| {
                    TrainNickname::parse(&value).map_err(|_| rusqlite::Error::InvalidQuery)
                })
                .transpose()?;
            let status_kind: String = row.get(3)?;
            let status_ref: String = row.get(4)?;
            let status = match status_kind.as_str() {
                "ready" => TrainStatus::Ready {
                    at: RailStationId::parse(&status_ref)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                "travelling" => TrainStatus::Travelling {
                    journey_id: JourneyId::parse(&status_ref)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(Train {
                id: row_domain_id(row, 0, "Train ID", TrainId::parse)?,
                evn,
                nickname,
                status,
                model_id: TrainModelId::new(row.get::<_, String>(5)?),
                original_purchase_price: Money::from_cents(row.get(6)?),
            })
        },
    )?;

    let mut services = query_all(
        connection,
        "SELECT id, name FROM passenger_services ORDER BY sequence",
        path,
        |row| {
            Ok(PassengerService {
                id: row_domain_id(row, 0, "Passenger Service ID", ServiceId::parse)?,
                name: row.get(1)?,
                stop_station_ids: Vec::new(),
                rail_line_ids: Vec::new(),
            })
        },
    )?;
    for service in &mut services {
        let mut stop_statement = connection
            .prepare("SELECT station_id FROM service_stops WHERE service_id = ?1 ORDER BY sequence")
            .map_err(|source| db_error("prepare Passenger Service stop query for", path, source))?;
        let stop_rows = stop_statement
            .query_map(params![service.id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| db_error("read Passenger Service stops from", path, source))?;
        for row in stop_rows {
            let station =
                row.map_err(|source| db_error("read Passenger Service stop from", path, source))?;
            service.stop_station_ids.push(
                RailStationId::parse(&station)
                    .map_err(|_| invalid_value(path, "Rail Station ID"))?,
            );
        }

        let mut statement = connection
            .prepare(
                "SELECT rail_line_id FROM service_lines WHERE service_id = ?1 ORDER BY sequence",
            )
            .map_err(|source| db_error("prepare Passenger Service path query for", path, source))?;
        let rows = statement
            .query_map(params![service.id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| db_error("read Passenger Service path from", path, source))?;
        for row in rows {
            let line =
                row.map_err(|source| db_error("read Passenger Service path from", path, source))?;
            service
                .rail_line_ids
                .push(RailLineId::parse(&line).map_err(|_| invalid_value(path, "Rail Line ID"))?);
        }
    }

    let demand = query_all(
        connection,
        "SELECT origin_station_id, destination_station_id, waiting_passengers, market_maturity_basis_points, passenger_arrival_rate_per_hour, fractional_passenger_seconds FROM origin_destination_demand ORDER BY sequence",
        path,
        |row| {
            Ok(OriginDestinationDemand {
                origin_station_id: row_domain_id(row, 0, "Demand origin", RailStationId::parse)?,
                destination_station_id: row_domain_id(
                    row,
                    1,
                    "Demand destination",
                    RailStationId::parse,
                )?,
                waiting_passengers: u32::try_from(row.get::<_, i64>(2)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                market_maturity: MarketMaturity::from_basis_points(row.get(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                passenger_arrival_rate_per_hour: PassengerArrivalRate::new(row.get(4)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                fractional_passenger_seconds: row_u64(
                    row,
                    5,
                    "Demand fractional passenger seconds",
                )?,
            })
        },
    )?;

    let mut active_journeys = query_all(
        connection,
        "SELECT id, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at FROM active_journeys ORDER BY sequence",
        path,
        |row| {
            Ok(Journey {
                id: row_domain_id(row, 0, "Journey ID", JourneyId::parse)?,
                service_id: row_domain_id(row, 1, "Passenger Service ID", ServiceId::parse)?,
                train_id: row_domain_id(row, 2, "Train ID", TrainId::parse)?,
                origin_station_id: row_domain_id(row, 3, "Journey origin", RailStationId::parse)?,
                destination_station_id: row_domain_id(
                    row,
                    4,
                    "Journey destination",
                    RailStationId::parse,
                )?,
                passengers_carried: u32::try_from(row.get::<_, i64>(5)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                fare: Money::from_cents(row.get(6)?),
                operating_revenue: Money::from_cents(row.get(7)?),
                credited_revenue: Money::from_cents(row.get(8)?),
                infrastructure_access_fee: Money::from_cents(row.get(9)?),
                fuel_cost: Money::from_cents(row.get(10)?),
                current_stop_index: usize::try_from(row.get::<_, i64>(11)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                passenger_groups: Vec::new(),
                departed_at: UtcSeconds::from_unix_seconds(row.get(12)?),
                arrives_at: UtcSeconds::from_unix_seconds(row.get(13)?),
            })
        },
    )?;
    for journey in &mut active_journeys {
        let mut statement = connection
            .prepare(
                "SELECT origin_station_id, destination_station_id, passengers, fare_cents
             FROM journey_passenger_groups
             WHERE journey_id = ?1
             ORDER BY sequence",
            )
            .map_err(|source| {
                db_error("prepare Journey passenger group query for", path, source)
            })?;
        let rows = statement
            .query_map(params![journey.id.to_string()], |row| {
                Ok(JourneyPassengerGroup {
                    origin_station_id: row_domain_id(
                        row,
                        0,
                        "Journey passenger origin",
                        RailStationId::parse,
                    )?,
                    destination_station_id: row_domain_id(
                        row,
                        1,
                        "Journey passenger destination",
                        RailStationId::parse,
                    )?,
                    passengers: u32::try_from(row.get::<_, i64>(2)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    fare: Money::from_cents(row.get(3)?),
                })
            })
            .map_err(|source| db_error("read Journey passenger groups from", path, source))?;
        for row in rows {
            journey.passenger_groups.push(
                row.map_err(|source| db_error("read Journey passenger group from", path, source))?,
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
             ORDER BY completed_at DESC, journey_id DESC
             LIMIT {RECENT_RECEIPT_LIMIT}
         )
         ORDER BY completed_at, journey_id"
    );
    let receipts = query_all(connection, &receipt_sql, path, |row| {
        Ok(JourneyReceipt {
            journey_id: row_domain_id(row, 0, "Journey receipt ID", JourneyId::parse)?,
            revenue: Money::from_cents(row.get(1)?),
            infrastructure_access_fee: Money::from_cents(row.get(2)?),
            fuel_cost: Money::from_cents(row.get(3)?),
            train_id: optional_row_domain_id(row, 4, "Journey receipt Train ID", TrainId::parse)?,
            train_model_name: row.get(5)?,
            origin_station_id: optional_row_domain_id(
                row,
                6,
                "Journey receipt origin",
                RailStationId::parse,
            )?,
            destination_station_id: optional_row_domain_id(
                row,
                7,
                "Journey receipt destination",
                RailStationId::parse,
            )?,
            passengers_carried: optional_row_u32(row, 8, "Journey receipt passengers")?,
            passenger_capacity: optional_row_u32(row, 9, "Journey receipt passenger capacity")?,
            completed_at: row
                .get::<_, Option<i64>>(10)?
                .map(UtcSeconds::from_unix_seconds),
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
            population: from_db_u64(region_population, "Region Population")
                .map_err(|field| invalid_value(path, field))?,
            settlements,
            rail_authority: RailAuthority {
                name: authority_name,
                rail_network: RailNetwork {
                    rail_stations,
                    rail_lines,
                },
                finances: rail_authority_finances,
                construction_capacity: u32::try_from(authority_construction_capacity)
                    .map_err(|_| invalid_value(path, "Rail Authority construction capacity"))?,
                infrastructure_projects,
            },
        },
        player_company: PlayerCompany {
            name: company_name,
            vehicle_keeper_mark: company_vkm,
            funds: Money::from_cents(company_funds),
            fleet: Fleet {
                trains,
                next_train_display_number: from_db_u64(
                    next_train_display_number,
                    "next Train display number",
                )
                .map_err(|field| invalid_value(path, field))?,
                next_evn_unit_by_model,
            },
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
                MoneyPerKilometre::new(access_rate)
                    .map_err(|_| invalid_value(path, "access fee rate"))?,
                Money::from_cents(starting_funds),
            ),
            demand: DemandRules {
                cap_duration: DurationSeconds::from_seconds(
                    from_db_u64(demand_cap, "demand cap duration")
                        .map_err(|field| invalid_value(path, field))?,
                ),
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
        .any(|settlement| !settlement.id.is_v4())
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
        .any(|station| !station.id.is_v4())
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
    if network.rail_lines.iter().any(|line| !line.id.is_v4()) {
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
    validate_infrastructure_projects(
        &state.region.rail_authority,
        &settlement_ids,
        &station_ids,
        &line_ids,
    )?;
    validate_rail_authority_finances(&state.region.rail_authority)?;

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
        .any(|service| !service.id.is_v4())
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
        .any(|train| !train.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId { kind: "Train" });
    }
    let highest_train_display_number = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| train.id.get())
        .max()
        .unwrap_or(0);
    if state.player_company.fleet.next_train_display_number == 0
        || state.player_company.fleet.next_train_display_number <= highest_train_display_number
    {
        return Err(SaveValidationError::InvalidValue {
            field: "next Train display number",
        });
    }

    for (model_id, next_unit_number) in &state.player_company.fleet.next_evn_unit_by_model {
        if train_catalogue().by_id(model_id).is_none()
            || *next_unit_number == 0
            || *next_unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER + 1
        {
            return Err(SaveValidationError::InvalidValue {
                field: "next EVN unit number",
            });
        }
    }
    for train in &state.player_company.fleet.trains {
        let Some(next_unit_number) = state
            .player_company
            .fleet
            .next_evn_unit_by_model
            .get(&train.model_id)
        else {
            return Err(SaveValidationError::InvalidValue {
                field: "next EVN unit number",
            });
        };
        if train.evn.unit_number() >= *next_unit_number {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Train EVN unit number has not been consumed by its model sequence",
            });
        }
    }

    let journey_ids = unique_ids(
        state.active_journeys.iter().map(|journey| journey.id),
        "Journey",
    )?;
    if state
        .active_journeys
        .iter()
        .any(|journey| !journey.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId { kind: "Journey" });
    }

    validate_train_statuses(
        &state.player_company.fleet.trains,
        state.region.railway_registration.numeric_code,
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

fn validate_rail_authority_finances(authority: &RailAuthority) -> Result<(), SaveValidationError> {
    if authority.construction_capacity == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "Rail Authority construction capacity",
        });
    }
    if authority.active_construction_count() > authority.construction_capacity {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority active construction exceeds its capacity",
        });
    }
    if authority.reserved_construction_count() > authority.construction_capacity {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority scheduled and active construction exceeds its capacity",
        });
    }

    let finances = &authority.finances;
    if finances.treasury.cents() < 0
        || finances.maintenance_reserve.cents() < 0
        || finances.committed_investment.cents() < 0
        || finances.carried_over_funds.cents() < 0
        || finances.regional_public_allocation.cents() < 0
        || finances.infrastructure_access_fee_revenue.cents() < 0
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Rail Authority financial amount",
        });
    }

    let earmarked = finances
        .maintenance_reserve
        .checked_add(finances.committed_investment)?;
    if earmarked > finances.treasury {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority earmarks exceed its treasury",
        });
    }
    if finances.carried_over_funds > finances.treasury {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority carry-over exceeds its treasury",
        });
    }

    Ok(())
}

fn validate_infrastructure_projects(
    authority: &RailAuthority,
    settlement_ids: &HashSet<SettlementId>,
    station_ids: &HashSet<RailStationId>,
    line_ids: &HashSet<RailLineId>,
) -> Result<(), SaveValidationError> {
    let project_ids = unique_ids(
        authority
            .infrastructure_projects
            .iter()
            .map(|project| project.id),
        "Infrastructure Project",
    )?;
    if project_ids.iter().any(|id| !id.is_v4()) {
        return Err(SaveValidationError::InvalidId {
            kind: "Infrastructure Project",
        });
    }

    let mut total_project_commitments = Money::ZERO;
    for project in &authority.infrastructure_projects {
        if project.funding.estimated_cost.cents() < 0
            || project.funding.authority_committed.cents() < 0
            || project.funding.operator_contributed.cents() < 0
            || project.funding.access_fee_credit_awarded.cents() < 0
            || project.funding.access_fee_credit_remaining.cents() < 0
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Infrastructure Project funding amount",
            });
        }
        if project.funding.total_funded()? > project.funding.estimated_cost {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project funding exceeds estimated cost",
            });
        }
        if project.funding.operator_contributed > project.funding.operator_contribution_cap()? {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project operator contribution exceeds its cap",
            });
        }
        if project.funding.access_fee_credit_remaining > project.funding.access_fee_credit_awarded {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project access credit remaining exceeds awarded credit",
            });
        }
        if !matches!(
            project.status,
            InfrastructureProjectStatus::Open | InfrastructureProjectStatus::Cancelled
        ) {
            total_project_commitments =
                total_project_commitments.checked_add(project.funding.authority_committed)?;
        }
    }
    if total_project_commitments > authority.finances.committed_investment {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Infrastructure Project commitments exceed Authority committed investment",
        });
    }

    let mut reserved_station_ids = HashSet::new();
    let mut reserved_line_ids = HashSet::new();
    for project in &authority.infrastructure_projects {
        match &project.kind {
            InfrastructureProjectKind::NewLine {
                planned_stations,
                planned_lines,
            } => {
                if planned_lines.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "New Line planned Rail Lines",
                    });
                }
                let local_planned_station_ids = unique_ids(
                    planned_stations.iter().map(|station| station.id),
                    "planned Rail Station",
                )?;
                for station in planned_stations {
                    if !station.id.is_v4()
                        || (project.status != InfrastructureProjectStatus::Open
                            && station_ids.contains(&station.id))
                        || !reserved_station_ids.insert(station.id)
                    {
                        return Err(SaveValidationError::InvalidId {
                            kind: "planned Rail Station",
                        });
                    }
                    if !settlement_ids.contains(&station.settlement_id) {
                        return Err(SaveValidationError::DanglingReference {
                            field: "planned Rail Station Settlement",
                        });
                    }
                }
                unique_ids(
                    planned_lines.iter().map(|line| line.id),
                    "planned Rail Line",
                )?;
                for line in planned_lines {
                    if !line.id.is_v4()
                        || (project.status != InfrastructureProjectStatus::Open
                            && line_ids.contains(&line.id))
                        || !reserved_line_ids.insert(line.id)
                    {
                        return Err(SaveValidationError::InvalidId {
                            kind: "planned Rail Line",
                        });
                    }
                    let endpoint_exists = |station_id: RailStationId| {
                        station_ids.contains(&station_id)
                            || local_planned_station_ids.contains(&station_id)
                    };
                    if !endpoint_exists(line.first_station_id)
                        || !endpoint_exists(line.second_station_id)
                    {
                        return Err(SaveValidationError::DanglingReference {
                            field: "planned Rail Line endpoint",
                        });
                    }
                    if line.first_station_id == line.second_station_id {
                        return Err(SaveValidationError::ImpossibleState {
                            reason: "a planned Rail Line has the same Rail Station at both endpoints",
                        });
                    }
                }
            }
            InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
            | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
            | InfrastructureProjectKind::Electrification { rail_line_ids }
            | InfrastructureProjectKind::Renewal { rail_line_ids } => {
                if rail_line_ids.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "Infrastructure Project Rail Lines",
                    });
                }
                unique_ids(
                    rail_line_ids.iter().copied(),
                    "Infrastructure Project Rail Line",
                )?;
                if rail_line_ids.iter().any(|id| !line_ids.contains(id)) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "Infrastructure Project Rail Line",
                    });
                }
            }
            InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
                if rail_station_ids.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "Infrastructure Project Rail Stations",
                    });
                }
                unique_ids(
                    rail_station_ids.iter().copied(),
                    "Infrastructure Project Rail Station",
                )?;
                if rail_station_ids.iter().any(|id| !station_ids.contains(id)) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "Infrastructure Project Rail Station",
                    });
                }
            }
        }
    }

    for (index, project) in authority.infrastructure_projects.iter().enumerate() {
        if project.status == InfrastructureProjectStatus::Scheduled {
            if !project.funding.is_fully_funded()
                || project.timeline.funding_completed_at.is_none()
                || project.timeline.scheduled_start_at.is_none()
            {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "scheduled Infrastructure Project is not fully funded and scheduled",
                });
            }
        }

        if project.status == InfrastructureProjectStatus::Construction {
            let (Some(scheduled_start), Some(construction_started), Some(planned_completion)) = (
                project.timeline.scheduled_start_at,
                project.timeline.construction_started_at,
                project.timeline.planned_completion_at,
            ) else {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "Infrastructure Project under construction is missing construction timestamps",
                });
            };
            if !project.funding.is_fully_funded()
                || project.timeline.funding_completed_at.is_none()
                || construction_started < scheduled_start
                || planned_completion <= construction_started
            {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "Infrastructure Project has an invalid construction lifecycle",
                });
            }
        }

        if !project.status.reserves_construction_capacity() {
            continue;
        }
        if authority.infrastructure_projects[index + 1..]
            .iter()
            .any(|other| {
                other.status.reserves_construction_capacity() && project.conflicts_with(other)
            })
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "conflicting Infrastructure Projects reserve construction at the same time",
            });
        }
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
        let cap = waiting_passenger_cap(state, demand);
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
        if !receipt.journey_id.is_v4() || !receipt_ids.insert(receipt.journey_id) {
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
            let train_id = receipt
                .train_id
                .expect("complete receipt context has Train ID");
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
            if !train_id.is_v4()
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
    registration_code: u8,
    station_ids: &HashSet<RailStationId>,
    journey_ids: &HashSet<crate::model::JourneyId>,
) -> Result<(), SaveValidationError> {
    let mut travelling_journey_ids = HashSet::new();
    let mut vehicle_numbers = HashSet::new();
    for train in trains {
        let Some(model) = model_for_train(train) else {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue model reference",
            });
        };
        if train.model_id.as_str().trim().is_empty() || train.original_purchase_price.cents() <= 0 {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue model reference",
            });
        }
        let evn = EuropeanVehicleNumber::parse(train.evn.as_str()).map_err(|_| {
            SaveValidationError::InvalidValue {
                field: "European Vehicle Number",
            }
        })?;
        if !vehicle_numbers.insert(evn.as_str().to_owned()) {
            return Err(SaveValidationError::DuplicateId {
                kind: "European Vehicle Number",
            });
        }
        if evn.vehicle_type_code() != model.evn_type_code()
            || evn.registration_code() != registration_code
            || evn.series_code() != model.evn_series_code()
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Train European Vehicle Number does not match its model and Region",
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

        onboard_passengers = onboard_passengers.checked_add(group.passengers).ok_or(
            SaveValidationError::Calculation(CalculationError::Overflow {
                operation: "Journey onboard passenger count",
            }),
        )?;
        onboard_revenue =
            onboard_revenue.checked_add(group.fare.checked_mul(u64::from(group.passengers))?)?;
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
    let first_station_id = *service.stop_station_ids.get(first_stop_index).ok_or(
        SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        },
    )?;
    let second_station_id = *service.stop_station_ids.get(second_stop_index).ok_or(
        SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        },
    )?;
    let line_ids =
        path_between_stations(network, first_station_id, second_station_id).map_err(|_| {
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
        };
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
                timeline: timeline(2_004),
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

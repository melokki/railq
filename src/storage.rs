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
mod slot;
mod state_io;
mod validation;

pub use self::slot::{SaveCodecError, SaveSlot, SaveSlotError};
pub use self::validation::{SaveValidationError, validate_game_state};

use self::schema::SCHEMA;

#[cfg(test)]
use self::migrations::migrate_v9_to_v10;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    hash::Hash,
    io,
    path::Path,
};

use rusqlite::{Connection, Transaction, params};

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


/// Number of settled Journey receipts kept in the live in-memory state.
/// SQLite retains the complete history for future statistics and reports.
const RECENT_RECEIPT_LIMIT: usize = 100;

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
mod tests;

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
mod support;
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
        AuthorityRules, BulletinCategory, BulletinEntry, CalculationError, ConstructionDifficulty,
        DemandRules, DistanceMetres, DurationSeconds, Electrification, EuropeanVehicleNumber,
        Financials, Fleet, GameRules, GameState, InfrastructureProject,
        InfrastructureProjectFunding, InfrastructureProjectId, InfrastructureProjectKind,
        InfrastructureProjectStatus, InfrastructureProjectTimeline, Journey, JourneyId,
        JourneyPassengerGroup, JourneyPurpose, JourneyReceipt, MarketMaturity, Money,
        MoneyPerKilometre, OriginDestinationDemand, PassengerArrivalRate, PassengerService,
        PlannedRailLine, PlannedRailStation, PlayerCompany, RailAuthority, RailAuthorityFinances,
        RailLine, RailLineId, RailNetwork, RailStation, RailStationId, RailwayRegistration, Region,
        ServiceDirectionMode, ServiceId, Settlement, SettlementId, SpeedKilometresPerHour,
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
pub const SAVE_VERSION: u32 = 37;

/// The local SQLite save used when no explicit path is supplied.
pub const DEFAULT_SAVE_PATH: &str = "railq.db";

/// Previous default RON save. `open_default` imports it once when no database
/// exists yet, leaving the source file untouched as a player-visible backup.
pub const LEGACY_SAVE_PATH: &str = "railq.ron";

use self::support::*;

#[cfg(test)]
mod tests;

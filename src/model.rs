//! Core domain value types.
//!
//! All calculations use integer units. When a calculation has a fractional
//! result, RailQ rounds up: a partial cent or partial second is charged or
//! scheduled as one whole unit. This keeps quotes deterministic and never
//! understates a cost or Journey duration.

mod ids;

pub use ids::{
    InfrastructureProjectId, JourneyId, RailLineId, RailStationId, ServiceId, SettlementId,
    TrainId, TrainModelId,
};

mod units;

pub use units::{
    CalculationError, ConstructionDifficulty, DistanceMetres, DurationSeconds, Electrification,
    Money, MoneyPerKilometre, PassengerCapacity, SpeedKilometresPerHour, SpeedMetresPerSecond,
    TrackCount, UtcSeconds, ValidationError,
};

mod vehicle;

pub use vehicle::{
    EuropeanVehicleNumber, EuropeanVehicleNumberError, TrainNickname, TrainNicknameError,
    VehicleKeeperMark, VehicleKeeperMarkError,
};

mod world;

pub use world::{
    BulletinCategory, BulletinEntry, RailwayRegistration, Region, Settlement, WorldPosition,
};

mod authority;

pub use authority::{
    InfrastructureProject, InfrastructureProjectFunding, InfrastructureProjectKind,
    InfrastructureProjectStatus, InfrastructureProjectTimeline, PROVISIONAL_CONSTRUCTION_CAPACITY,
    PROVISIONAL_MAINTENANCE_RESERVE_PER_TRACK_KILOMETRE,
    PROVISIONAL_OPERATOR_ACCESS_CREDIT_PERCENT, PROVISIONAL_OPERATOR_CONTRIBUTION_CAP_PERCENT,
    PROVISIONAL_OPERATOR_CONTRIBUTION_TRANCHE_PERCENT, PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION,
    PlannedRailLine, PlannedRailStation, RailAuthority, RailAuthorityFinances, RailLine,
    RailNetwork, RailStation, next_utc_midnight_after,
};

mod company;

pub use company::{Fleet, PlayerCompany, Train, TrainStatus};

mod operations;

pub use operations::{
    Journey, JourneyPassengerGroup, JourneyPurpose, MarketMaturity, OriginDestinationDemand,
    PassengerArrivalRate, PassengerService, ServiceDirectionMode,
};

mod finance;

pub use finance::{DemandRules, Financials, GameRules, JourneyReceipt};

mod game;

pub use game::GameState;

#[cfg(test)]
mod tests;

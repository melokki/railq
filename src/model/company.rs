use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    EuropeanVehicleNumber, JourneyId, Money, PassengerService, RailStationId, TrainId,
    TrainModelId, TrainNickname, VehicleKeeperMark,
};

/// The passenger railway company controlled by the player.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlayerCompany {
    pub name: String,
    pub vehicle_keeper_mark: VehicleKeeperMark,
    pub funds: Money,
    pub fleet: Fleet,
    pub passenger_services: Vec<PassengerService>,
}

/// All Trains owned by the Player Company.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Fleet {
    pub trains: Vec<Train>,
    /// Next compact display number for an owned Train. This is not the Train
    /// identity; persisted Train IDs are UUID v4 values.
    #[serde(default = "default_next_train_display_number")]
    pub next_train_display_number: u64,
    /// Next EVN unit number to allocate for each persistent Train model.
    ///
    /// Values are one-based; 1000 means the model's 001–999 allocation is
    /// exhausted. Keeping the counter after resale prevents EVN reuse.
    #[serde(default)]
    pub next_evn_unit_by_model: BTreeMap<TrainModelId, u16>,
}

const fn default_next_train_display_number() -> u64 {
    1
}

impl Default for Fleet {
    fn default() -> Self {
        Self {
            trains: Vec::new(),
            next_train_display_number: default_next_train_display_number(),
            next_evn_unit_by_model: BTreeMap::new(),
        }
    }
}

/// Passenger rolling stock owned by the Player Company.
///
/// Immutable technical specifications live in the central Train catalogue. An
/// owned Train persists only the stable catalogue model ID plus instance state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Train {
    pub id: TrainId,
    /// Permanent official vehicle number assigned when the Train joins the Fleet.
    pub evn: EuropeanVehicleNumber,
    /// Optional player-facing name. It never changes the official EVN.
    #[serde(default)]
    pub nickname: Option<TrainNickname>,
    pub status: TrainStatus,
    pub model_id: TrainModelId,
    /// The amount actually paid when this Train joined the Fleet.
    ///
    /// Resale is calculated from this historical purchase price, not the
    /// current catalogue price.
    pub original_purchase_price: Money,
}

/// The mutually exclusive operating status of a Train.
///
/// A Train is either ready at a Rail Station or travelling on one Journey;
/// the enum representation makes it impossible to represent both at once.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TrainStatus {
    Ready { at: RailStationId },
    Travelling { journey_id: JourneyId },
}

//! Region, geography, and persistent world-facing domain types.

use serde::{Deserialize, Serialize};

use super::{RailAuthority, SettlementId, UtcSeconds};

/// The fictional place in which a game takes place.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Region {
    pub name: String,
    /// Stable fictional railway registration identity used for vehicle numbering.
    #[serde(default)]
    pub railway_registration: RailwayRegistration,
    /// The total Population of every Settlement in this Region.
    pub population: u64,
    pub settlements: Vec<Settlement>,
    /// Persistent significant regional railway developments shown in the Bulletin workspace.
    #[serde(default)]
    pub bulletin: Vec<BulletinEntry>,
    pub rail_authority: RailAuthority,
}

/// High-level source/type of one persistent regional Bulletin item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BulletinCategory {
    Local,
    Authority,
    Construction,
    Network,
}

/// One persistent, player-facing record of a significant railway-world event.
///
/// Routine Train movements deliberately do not belong here; the Bulletin is a
/// compact history of developments that materially change or explain the world.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BulletinEntry {
    pub occurred_at: UtcSeconds,
    pub category: BulletinCategory,
    pub headline: String,
    pub detail: String,
}

/// Stable fictional registration identity assigned when a Region is generated.
///
/// `numeric_code` is deliberately two digits so it can later occupy the
/// country-code position of RailQ's EVN-style vehicle numbers. `mark` is the
/// short alphabetic Region marking shown alongside that identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailwayRegistration {
    pub numeric_code: u8,
    pub mark: String,
}

impl Default for RailwayRegistration {
    fn default() -> Self {
        Self {
            numeric_code: 99,
            mark: "RQ".into(),
        }
    }
}

impl RailwayRegistration {
    pub fn display_code(&self) -> String {
        format!("{:02}", self.numeric_code)
    }
}

/// Stable geographical position inside the generated Region.
///
/// Coordinates are world-space kilometres, not terminal cells. The UI may
/// scale them to any terminal size while simulation systems can derive
/// consistent physical distances from the same geography.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorldPosition {
    pub x: i32,
    pub y: i32,
}

impl WorldPosition {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A populated place in a Region, with or without railway access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settlement {
    pub id: SettlementId,
    pub name: String,
    pub population: u64,
    #[serde(default)]
    pub position: WorldPosition,
}

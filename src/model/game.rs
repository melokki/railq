use serde::{Deserialize, Serialize};

use super::{
    Financials, GameRules, Journey, OriginDestinationDemand, PlayerCompany, Region, UtcSeconds,
};

/// The complete mutable state of one RailQ game.
///
/// The Region owns public infrastructure through its Rail Authority. The
/// Player Company separately owns its Fleet and Passenger Services. Active
/// Journeys and origin-destination demand belong to the game because they
/// describe the current operating state rather than either owner's assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameState {
    /// The seed that generated this game's Region. It is saved so a loaded
    /// game never needs to regenerate its world.
    pub world_seed: u64,
    pub region: Region,
    pub player_company: PlayerCompany,
    pub origin_destination_demand: Vec<OriginDestinationDemand>,
    pub active_journeys: Vec<Journey>,
    pub financials: Financials,
    pub rules: GameRules,
    pub last_processed_at: UtcSeconds,
}

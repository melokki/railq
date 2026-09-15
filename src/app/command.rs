//! Application commands issued by presentation adapters.
//!
//! Commands contain player intent only. Runtime concerns such as the current
//! timestamp are supplied separately when the application executes them.

use crate::model::{TrainId, TrainNickname};

/// A player-requested state transition accepted by the application layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommand {
    /// Change or clear the player-facing nickname of one owned Train.
    UpdateTrainNickname {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
}

/// The typed outcome of a successfully executed application command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommandResult {
    /// One Train nickname change was durably committed.
    TrainNicknameUpdated { train_id: TrainId },
}

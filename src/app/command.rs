//! Application commands issued by presentation adapters.
//!
//! Commands contain player intent only. Runtime concerns such as the current
//! timestamp are supplied separately when the application executes them.

use crate::model::{
    InfrastructureProjectId, Money, RailStationId, TrainId, TrainNickname, VehicleKeeperMark,
};

/// A player-requested state transition accepted by the application layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommand {
    /// Purchase one catalogue Train and deliver it to an existing station.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
    /// Sell one READY owned Train.
    SellTrain { train_id: TrainId },
    /// Contribute Player Company funds to one Authority infrastructure project.
    ContributeInfrastructure {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
    /// Change the Player Company's Vehicle Keeper Mark.
    UpdateCompanyVkm {
        vehicle_keeper_mark: VehicleKeeperMark,
    },
    /// Change or clear the player-facing nickname of one owned Train.
    UpdateTrainNickname {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
}

/// The typed outcome of a successfully executed application command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommandResult {
    /// One Train purchase was durably committed.
    TrainPurchased { train_id: TrainId },
    /// One Train sale was durably committed.
    TrainSold { train_id: TrainId, proceeds: Money },
    /// One infrastructure contribution was durably committed.
    InfrastructureContributionRecorded {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
    /// The Player Company VKM was durably committed.
    CompanyVkmUpdated,
    /// One Train nickname change was durably committed.
    TrainNicknameUpdated { train_id: TrainId },
}

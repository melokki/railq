//! Application commands issued by presentation adapters.
//!
//! Commands contain player intent only. Runtime concerns such as the current
//! timestamp are supplied separately when the application executes them.

use crate::model::{
    InfrastructureProjectId, JourneyId, Money, RailStationId, ServiceDirectionMode, ServiceId,
    TrainId, TrainNickname, VehicleKeeperMark,
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
    /// Create one persistent Passenger Service route pattern.
    CreatePassengerService {
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
    },
    /// Update the stop pattern of one unused Passenger Service.
    UpdatePassengerService {
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
    },
    /// Change or clear the optional commercial name of one Passenger Service.
    UpdatePassengerServiceName {
        service_id: ServiceId,
        custom_name: Option<String>,
    },
    /// Delete one unused Passenger Service.
    DeletePassengerService { service_id: ServiceId },
    /// Persistently allocate one owned Train to a Passenger Service.
    AssignTrainToService {
        train_id: TrainId,
        service_id: ServiceId,
    },
    /// Clear one owned Train's persistent Passenger Service allocation.
    UnassignTrainFromService { train_id: TrainId },
    /// Authorise one Manual Dispatch over an existing Passenger Service.
    ManualDispatch {
        train_id: TrainId,
        service_id: ServiceId,
    },
    /// Move one assigned Train empty to a valid departure terminus.
    PositionTrainForService {
        train_id: TrainId,
        service_id: ServiceId,
        destination_station_id: RailStationId,
    },
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
    /// Persist that the player has opened the Bulletin through the current entry count.
    AcknowledgeBulletin { seen_count: u64 },
}

/// The typed outcome of a successfully executed application command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommandResult {
    /// One Train purchase was durably committed.
    TrainPurchased { train_id: TrainId },
    /// One Train sale was durably committed.
    TrainSold { train_id: TrainId, proceeds: Money },
    /// One Passenger Service creation was durably committed.
    PassengerServiceCreated { service_id: ServiceId },
    /// One Passenger Service update was durably committed.
    PassengerServiceUpdated { service_id: ServiceId },
    /// One Passenger Service naming change was durably committed.
    PassengerServiceRenamed { service_id: ServiceId },
    /// One Passenger Service deletion was durably committed.
    PassengerServiceDeleted { service_id: ServiceId },
    /// One Train-to-Service assignment was durably committed.
    TrainServiceAssigned {
        train_id: TrainId,
        service_id: ServiceId,
    },
    /// One Train-to-Service assignment was durably cleared.
    TrainServiceUnassigned { train_id: TrainId },
    /// One Manual Dispatch was durably committed.
    JourneyDispatched { journey_id: JourneyId },
    /// One empty positioning Journey was durably committed.
    TrainPositioningStarted { journey_id: JourneyId },
    /// One infrastructure contribution was durably committed.
    InfrastructureContributionRecorded {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
    /// The Player Company VKM was durably committed.
    CompanyVkmUpdated,
    /// One Train nickname change was durably committed.
    TrainNicknameUpdated { train_id: TrainId },
    /// The Bulletin read boundary was durably committed.
    BulletinAcknowledged { seen_count: u64 },
}

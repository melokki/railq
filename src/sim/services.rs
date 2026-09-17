//! Passenger Service path lookup, creation, editing, and deletion.
//!
//! Services belong to the Player Company. They reuse Rail Authority-owned
//! Rail Lines and are deliberately distinct from individual directional Journeys.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
};

use crate::model::{
    GameState, PassengerService, RailLineId, RailNetwork, RailStationId, ServiceDirectionMode,
    ServiceId, SettlementId, TrainId, TrainStatus,
};

/// Why a Passenger Service path cannot be selected, created, or removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceError {
    /// A Service must contain at least two stops.
    TooFewStops,
    /// A Service cannot call at the same Rail Station more than once.
    DuplicateStop { station_id: RailStationId },
    /// A stop sequence would require backtracking over an already-used Rail Line.
    RepeatedRailLine { rail_line_id: RailLineId },
    /// A Service must connect two distinct Rail Stations.
    SameEndpoint { station_id: RailStationId },
    /// The selected Rail Station is not part of the Rail Network.
    RailStationNotFound { station_id: RailStationId },
    /// The selected Settlement does not exist in the Region.
    SettlementNotFound { settlement_id: SettlementId },
    /// The selected Settlement has no Rail Station.
    UnconnectedSettlement { settlement_id: SettlementId },
    /// Both endpoints exist, but no Rail Line path connects them.
    NoPath {
        first_station_id: RailStationId,
        second_station_id: RailStationId,
    },
    /// An identical ordered stop pattern already exists.
    DuplicateService { service_id: ServiceId },
    /// The selected Service does not exist.
    ServiceNotFound { service_id: ServiceId },
    /// An active Journey still references the selected Service.
    ServiceInUse { service_id: ServiceId },
    /// A new Service ID cannot be represented.
    ServiceIdExhausted,
    /// No further public train number can be allocated.
    TrainNumberExhausted,
    /// A commercial Service name is longer than the supported UI limit.
    ServiceNameTooLong,
    /// A commercial Service name contains a control character.
    InvalidServiceName,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewStops => {
                write!(formatter, "a Passenger Service requires at least two stops")
            }
            Self::DuplicateStop { station_id } => write!(
                formatter,
                "Rail Station {} is already a stop on this Passenger Service",
                station_id.get()
            ),
            Self::RepeatedRailLine { rail_line_id } => write!(
                formatter,
                "the selected stops would backtrack over Rail Line {}",
                rail_line_id.get()
            ),
            Self::SameEndpoint { station_id } => write!(
                formatter,
                "Rail Station {} cannot be both Service endpoints",
                station_id.get()
            ),
            Self::RailStationNotFound { station_id } => write!(
                formatter,
                "Rail Station {} is not in the Rail Network",
                station_id.get()
            ),
            Self::SettlementNotFound { settlement_id } => write!(
                formatter,
                "Settlement {} does not exist in the Region",
                settlement_id.get()
            ),
            Self::UnconnectedSettlement { settlement_id } => write!(
                formatter,
                "Settlement {} is unconnected to the Rail Network",
                settlement_id.get()
            ),
            Self::NoPath {
                first_station_id,
                second_station_id,
            } => write!(
                formatter,
                "no Rail Line path connects Rail Stations {} and {}",
                first_station_id.get(),
                second_station_id.get()
            ),
            Self::DuplicateService { service_id } => write!(
                formatter,
                "Passenger Service R{} already uses that ordered stop pattern",
                service_id.get()
            ),
            Self::ServiceNotFound { service_id } => {
                write!(
                    formatter,
                    "Passenger Service {} does not exist",
                    service_id.get()
                )
            }
            Self::ServiceInUse { service_id } => write!(
                formatter,
                "Passenger Service {} cannot be changed while a Journey is using it",
                service_id.get()
            ),
            Self::ServiceIdExhausted => write!(formatter, "Passenger Service IDs are exhausted"),
            Self::TrainNumberExhausted => {
                write!(formatter, "Passenger Service train numbers are exhausted")
            }
            Self::ServiceNameTooLong => write!(
                formatter,
                "Passenger Service name accepts up to {} visible characters",
                PassengerService::MAX_CUSTOM_NAME_CHARACTERS
            ),
            Self::InvalidServiceName => {
                write!(formatter, "Passenger Service name contains an invalid character")
            }
        }
    }
}

impl Error for ServiceError {}

/// Why a Train allocation to a Passenger Service cannot be changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAssignmentError {
    /// The selected Train is not owned by the Player Company.
    TrainNotFound { train_id: TrainId },
    /// The selected Passenger Service does not exist.
    ServiceNotFound { service_id: ServiceId },
    /// Allocation changes wait until an active Journey has finished.
    TrainTravelling {
        train_id: TrainId,
        journey_id: crate::model::JourneyId,
    },
}

impl fmt::Display for ServiceAssignmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TrainNotFound { train_id } => {
                write!(formatter, "Train {} is not in the Fleet", train_id.get())
            }
            Self::ServiceNotFound { service_id } => write!(
                formatter,
                "Passenger Service {} does not exist",
                service_id.get()
            ),
            Self::TrainTravelling {
                train_id,
                journey_id,
            } => write!(
                formatter,
                "Train {} is travelling on Journey {} and its Passenger Service assignment cannot change",
                train_id.get(),
                journey_id.get()
            ),
        }
    }
}

impl Error for ServiceAssignmentError {}

/// Allocates an owned READY Train to one Passenger Service.
///
/// Assignment is persistent administrative state and does not dispatch the
/// Train. A READY Train may be assigned regardless of its current Rail Station;
/// positioning remains an operational concern for dispatch. Re-applying the
/// same assignment is an idempotent no-op, including while the Train is moving.
pub fn assign_train_to_service(
    state: &mut GameState,
    train_id: TrainId,
    service_id: ServiceId,
) -> Result<(), ServiceAssignmentError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or(ServiceAssignmentError::TrainNotFound { train_id })?;
    if !state
        .player_company
        .passenger_services
        .iter()
        .any(|service| service.id == service_id)
    {
        return Err(ServiceAssignmentError::ServiceNotFound { service_id });
    }
    if state.player_company.fleet.assigned_service_id(train_id) == Some(service_id) {
        return Ok(());
    }
    if let TrainStatus::Travelling { journey_id } = train.status {
        return Err(ServiceAssignmentError::TrainTravelling {
            train_id,
            journey_id,
        });
    }

    state
        .player_company
        .fleet
        .service_assignments
        .insert(train_id, service_id);
    Ok(())
}

/// Clears a Train's Passenger Service allocation without changing its location.
///
/// A travelling Train keeps its current assignment until its active Journey
/// completes. Clearing an already-unassigned Train is an idempotent no-op.
pub fn unassign_train_from_service(
    state: &mut GameState,
    train_id: TrainId,
) -> Result<(), ServiceAssignmentError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or(ServiceAssignmentError::TrainNotFound { train_id })?;
    if state.player_company.fleet.assigned_service_id(train_id).is_none() {
        return Ok(());
    }
    if let TrainStatus::Travelling { journey_id } = train.status {
        return Err(ServiceAssignmentError::TrainTravelling {
            train_id,
            journey_id,
        });
    }

    state.player_company.fleet.service_assignments.remove(&train_id);
    Ok(())
}

/// Returns the ordered Rail Line path between two Rail Stations.
///
/// The returned order follows the requested direction. The starter Rail
/// Network is a tree, so the path is unique.
pub fn path_between_stations(
    network: &RailNetwork,
    first_station_id: RailStationId,
    second_station_id: RailStationId,
) -> Result<Vec<RailLineId>, ServiceError> {
    if first_station_id == second_station_id {
        return Err(ServiceError::SameEndpoint {
            station_id: first_station_id,
        });
    }

    let station_ids = network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect::<HashSet<_>>();
    if !station_ids.contains(&first_station_id) {
        return Err(ServiceError::RailStationNotFound {
            station_id: first_station_id,
        });
    }
    if !station_ids.contains(&second_station_id) {
        return Err(ServiceError::RailStationNotFound {
            station_id: second_station_id,
        });
    }

    let mut adjacent_lines = HashMap::<RailStationId, Vec<(RailStationId, RailLineId)>>::new();
    for line in &network.rail_lines {
        adjacent_lines
            .entry(line.first_station_id)
            .or_default()
            .push((line.second_station_id, line.id));
        adjacent_lines
            .entry(line.second_station_id)
            .or_default()
            .push((line.first_station_id, line.id));
    }

    let mut visited = HashSet::from([first_station_id]);
    let mut queue = VecDeque::from([first_station_id]);
    let mut previous = HashMap::<RailStationId, (RailStationId, RailLineId)>::new();

    while let Some(current_station_id) = queue.pop_front() {
        if current_station_id == second_station_id {
            break;
        }
        for &(next_station_id, rail_line_id) in adjacent_lines
            .get(&current_station_id)
            .into_iter()
            .flatten()
        {
            if visited.insert(next_station_id) {
                previous.insert(next_station_id, (current_station_id, rail_line_id));
                queue.push_back(next_station_id);
            }
        }
    }

    if !visited.contains(&second_station_id) {
        return Err(ServiceError::NoPath {
            first_station_id,
            second_station_id,
        });
    }

    let mut path = Vec::new();
    let mut current_station_id = second_station_id;
    while current_station_id != first_station_id {
        let (previous_station_id, rail_line_id) = previous
            .get(&current_station_id)
            .copied()
            .expect("a visited Rail Station other than the path origin has a predecessor");
        path.push(rail_line_id);
        current_station_id = previous_station_id;
    }
    path.reverse();
    Ok(path)
}

/// Builds the full Rail Line path for an ordered stop pattern.
///
/// Stops may skip intermediate Rail Stations. Backtracking over a Rail Line is
/// rejected so the Service remains a simple directional path.
pub fn service_path_for_stops(
    network: &RailNetwork,
    stop_station_ids: &[RailStationId],
) -> Result<Vec<RailLineId>, ServiceError> {
    if stop_station_ids.len() < 2 {
        return Err(ServiceError::TooFewStops);
    }

    let mut seen_stops = HashSet::new();
    for station_id in stop_station_ids {
        if !seen_stops.insert(*station_id) {
            return Err(ServiceError::DuplicateStop {
                station_id: *station_id,
            });
        }
    }

    let mut used_lines = HashSet::new();
    let mut path = Vec::new();
    for endpoints in stop_station_ids.windows(2) {
        let segment = path_between_stations(network, endpoints[0], endpoints[1])?;
        for rail_line_id in segment {
            if !used_lines.insert(rail_line_id) {
                return Err(ServiceError::RepeatedRailLine { rail_line_id });
            }
            path.push(rail_line_id);
        }
    }
    Ok(path)
}

/// Creates one named Passenger Service, bidirectional by default.
///
/// Names are intentionally generated from the persistent Service ID for now;
/// a later naming feature can change the display name without changing identity.
pub fn create_service(
    state: &mut GameState,
    stop_station_ids: Vec<RailStationId>,
) -> Result<ServiceId, ServiceError> {
    create_service_with_mode(
        state,
        stop_station_ids,
        ServiceDirectionMode::BothDirections,
    )
}

/// Creates one Passenger Service using the requested operating direction mode.
pub fn create_service_with_mode(
    state: &mut GameState,
    stop_station_ids: Vec<RailStationId>,
    direction_mode: ServiceDirectionMode,
) -> Result<ServiceId, ServiceError> {
    let rail_line_ids =
        service_path_for_stops(&state.region.rail_authority.rail_network, &stop_station_ids)?;

    if let Some(existing) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.stop_station_ids == stop_station_ids)
    {
        return Err(ServiceError::DuplicateService {
            service_id: existing.id,
        });
    }

    let service_id = ServiceId::new_v4();
    let service_name = next_service_name(&state.player_company.passenger_services);
    let (forward_train_number, reverse_train_number) =
        allocate_train_numbers(&state.player_company.passenger_services, direction_mode)?;
    state
        .player_company
        .passenger_services
        .push(PassengerService {
            id: service_id,
            name: service_name,
            custom_name: None,
            direction_mode,
            forward_train_number,
            reverse_train_number,
            stop_station_ids,
            rail_line_ids,
        });
    Ok(service_id)
}

fn next_service_name(services: &[PassengerService]) -> String {
    let next = services
        .iter()
        .filter_map(|service| service.name.strip_prefix('R')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    format!("R{next}")
}

const FIRST_PASSENGER_TRAIN_NUMBER: u32 = 100;

fn allocate_train_numbers(
    services: &[PassengerService],
    direction_mode: ServiceDirectionMode,
) -> Result<(u32, Option<u32>), ServiceError> {
    let forward = next_available_train_number(services)?;
    let reverse = match direction_mode {
        ServiceDirectionMode::BothDirections => Some(
            forward
                .checked_add(1)
                .ok_or(ServiceError::TrainNumberExhausted)?,
        ),
        ServiceDirectionMode::ForwardOnly => None,
    };
    Ok((forward, reverse))
}

fn next_available_train_number(services: &[PassengerService]) -> Result<u32, ServiceError> {
    let highest = services
        .iter()
        .flat_map(|service| {
            [
                Some(service.forward_train_number),
                service.reverse_train_number,
            ]
            .into_iter()
            .flatten()
        })
        .max();
    match highest {
        Some(number) => number
            .checked_add(1)
            .ok_or(ServiceError::TrainNumberExhausted),
        None => Ok(FIRST_PASSENGER_TRAIN_NUMBER),
    }
}

/// Updates the ordered stop pattern of an unused Passenger Service while
/// preserving its persistent identity and generated name.
///
/// Active Journeys retain references to the Service definition, so editing is
/// deliberately rejected until every Journey using the Service has arrived.
pub fn update_service(
    state: &mut GameState,
    service_id: ServiceId,
    stop_station_ids: Vec<RailStationId>,
) -> Result<(), ServiceError> {
    let direction_mode = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .map(|service| service.direction_mode)
        .ok_or(ServiceError::ServiceNotFound { service_id })?;
    update_service_with_mode(state, service_id, stop_station_ids, direction_mode)
}

/// Updates the route and direction mode of an unused Passenger Service.
pub fn update_service_with_mode(
    state: &mut GameState,
    service_id: ServiceId,
    stop_station_ids: Vec<RailStationId>,
    direction_mode: ServiceDirectionMode,
) -> Result<(), ServiceError> {
    let Some(index) = state
        .player_company
        .passenger_services
        .iter()
        .position(|service| service.id == service_id)
    else {
        return Err(ServiceError::ServiceNotFound { service_id });
    };

    if state
        .active_journeys
        .iter()
        .any(|journey| journey.service_id == service_id)
    {
        return Err(ServiceError::ServiceInUse { service_id });
    }

    let rail_line_ids =
        service_path_for_stops(&state.region.rail_authority.rail_network, &stop_station_ids)?;

    if let Some(existing) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id != service_id && service.stop_station_ids == stop_station_ids)
    {
        return Err(ServiceError::DuplicateService {
            service_id: existing.id,
        });
    }

    let current_mode = state.player_company.passenger_services[index].direction_mode;
    let reverse_train_number = match (current_mode, direction_mode) {
        (ServiceDirectionMode::BothDirections, ServiceDirectionMode::ForwardOnly) => None,
        (ServiceDirectionMode::ForwardOnly, ServiceDirectionMode::BothDirections) => {
            Some(next_available_train_number(
                &state.player_company.passenger_services,
            )?)
        }
        (_, ServiceDirectionMode::BothDirections) => {
            state.player_company.passenger_services[index].reverse_train_number
        }
        (_, ServiceDirectionMode::ForwardOnly) => None,
    };

    let service = &mut state.player_company.passenger_services[index];
    service.stop_station_ids = stop_station_ids;
    service.rail_line_ids = rail_line_ids;
    service.direction_mode = direction_mode;
    service.reverse_train_number = reverse_train_number;
    Ok(())
}

/// Finds or creates the direct two-stop Passenger Service in one direction.
///
/// This compatibility helper remains for tests, recovery logic, and legacy
/// callers. The player-facing Manual Dispatch flow selects an existing Service.
pub fn find_or_create_service(
    state: &mut GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> Result<ServiceId, ServiceError> {
    let stops = vec![origin_station_id, destination_station_id];
    if let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.stop_station_ids == stops)
    {
        return Ok(service.id);
    }
    create_service(state, stops)
}

/// Changes or clears the optional commercial name of one Passenger Service.
///
/// Naming is metadata only, so it is allowed while Trains are operating the
/// Service and never changes the generated `R` code or directional train numbers.
pub fn rename_service(
    state: &mut GameState,
    service_id: ServiceId,
    custom_name: Option<String>,
) -> Result<(), ServiceError> {
    let service = state
        .player_company
        .passenger_services
        .iter_mut()
        .find(|service| service.id == service_id)
        .ok_or(ServiceError::ServiceNotFound { service_id })?;

    let custom_name = match custom_name {
        Some(value) => {
            let normalized = value.trim();
            if normalized.is_empty() {
                None
            } else {
                if normalized.chars().count() > PassengerService::MAX_CUSTOM_NAME_CHARACTERS {
                    return Err(ServiceError::ServiceNameTooLong);
                }
                if normalized.chars().any(char::is_control) {
                    return Err(ServiceError::InvalidServiceName);
                }
                Some(normalized.to_owned())
            }
        }
        None => None,
    };
    service.custom_name = custom_name;
    Ok(())
}

/// Removes an unused Passenger Service.
pub fn delete_service(state: &mut GameState, service_id: ServiceId) -> Result<(), ServiceError> {
    if state
        .active_journeys
        .iter()
        .any(|journey| journey.service_id == service_id)
    {
        return Err(ServiceError::ServiceInUse { service_id });
    }

    let Some(index) = state
        .player_company
        .passenger_services
        .iter()
        .position(|service| service.id == service_id)
    else {
        return Err(ServiceError::ServiceNotFound { service_id });
    };
    state.player_company.passenger_services.remove(index);
    state
        .player_company
        .fleet
        .service_assignments
        .retain(|_, assigned_service_id| *assigned_service_id != service_id);
    Ok(())
}

/// Resolves selected Settlements to Rail Stations before creating or reusing a
/// direct Passenger Service. This rejects the Region's unconnected Settlements.
pub fn find_or_create_service_between_settlements(
    state: &mut GameState,
    first_settlement_id: SettlementId,
    second_settlement_id: SettlementId,
) -> Result<ServiceId, ServiceError> {
    if !state
        .region
        .settlements
        .iter()
        .any(|settlement| settlement.id == first_settlement_id)
    {
        return Err(ServiceError::SettlementNotFound {
            settlement_id: first_settlement_id,
        });
    }
    if !state
        .region
        .settlements
        .iter()
        .any(|settlement| settlement.id == second_settlement_id)
    {
        return Err(ServiceError::SettlementNotFound {
            settlement_id: second_settlement_id,
        });
    }

    let rail_stations = &state.region.rail_authority.rail_network.rail_stations;
    let first_station_id = rail_stations
        .iter()
        .find(|station| station.settlement_id == first_settlement_id)
        .map(|station| station.id)
        .ok_or(ServiceError::UnconnectedSettlement {
            settlement_id: first_settlement_id,
        })?;
    let second_station_id = rail_stations
        .iter()
        .find(|station| station.settlement_id == second_settlement_id)
        .map(|station| station.id)
        .ok_or(ServiceError::UnconnectedSettlement {
            settlement_id: second_settlement_id,
        })?;

    find_or_create_service(state, first_station_id, second_station_id)
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{
            JourneyId, Money, RailLineId, RailStationId, ServiceDirectionMode, SettlementId,
            TrainStatus, UtcSeconds,
        },
        sim::{fleet::purchase_train, world::create_new_game},
    };

    use super::{
        ServiceAssignmentError, ServiceError, assign_train_to_service, create_service,
        create_service_with_mode, delete_service, find_or_create_service,
        find_or_create_service_between_settlements, path_between_stations, rename_service,
        service_path_for_stops, unassign_train_from_service, update_service,
        update_service_with_mode,
    };

    fn game() -> crate::model::GameState {
        create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0))
    }

    #[test]
    fn a_multi_line_path_follows_each_requested_direction() {
        let game = game();
        let network = &game.region.rail_authority.rail_network;

        assert_eq!(
            path_between_stations(network, RailStationId::new(1), RailStationId::new(3)),
            Ok(vec![RailLineId::new(1), RailLineId::new(2)])
        );
        assert_eq!(
            path_between_stations(network, RailStationId::new(3), RailStationId::new(1)),
            Ok(vec![RailLineId::new(2), RailLineId::new(1)])
        );
    }

    #[test]
    fn ordered_stops_build_one_directional_path() {
        let game = game();
        let network = &game.region.rail_authority.rail_network;

        assert_eq!(
            service_path_for_stops(
                network,
                &[
                    RailStationId::new(1),
                    RailStationId::new(2),
                    RailStationId::new(3),
                ],
            ),
            Ok(vec![RailLineId::new(1), RailLineId::new(2)])
        );
    }

    #[test]
    fn repeated_stop_or_backtracking_is_rejected() {
        let game = game();
        let network = &game.region.rail_authority.rail_network;

        assert_eq!(
            service_path_for_stops(
                network,
                &[
                    RailStationId::new(1),
                    RailStationId::new(3),
                    RailStationId::new(2),
                ],
            ),
            Err(ServiceError::RepeatedRailLine {
                rail_line_id: RailLineId::new(2)
            })
        );
    }

    #[test]
    fn direct_services_are_directional() {
        let mut game = game();

        let forward =
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(3))
                .unwrap();
        let forward_again =
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(3))
                .unwrap();
        let reverse =
            find_or_create_service(&mut game, RailStationId::new(3), RailStationId::new(1))
                .unwrap();

        assert_eq!(forward, forward_again);
        assert_ne!(forward, reverse);
        assert_eq!(game.player_company.passenger_services.len(), 2);
    }

    #[test]
    fn explicit_service_persists_ordered_stops_and_generated_name() {
        let mut game = game();
        let service_id = create_service(
            &mut game,
            vec![
                RailStationId::new(1),
                RailStationId::new(2),
                RailStationId::new(3),
            ],
        )
        .unwrap();

        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.name, "R1");
        assert_eq!(service.forward_train_number, 100);
        assert_eq!(service.reverse_train_number, Some(101));
        assert_eq!(
            service.stop_station_ids,
            vec![
                RailStationId::new(1),
                RailStationId::new(2),
                RailStationId::new(3)
            ]
        );
    }

    #[test]
    fn one_way_service_uses_only_a_forward_train_number() {
        let mut game = game();

        let service_id = create_service_with_mode(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
            ServiceDirectionMode::ForwardOnly,
        )
        .unwrap();

        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.direction_mode, ServiceDirectionMode::ForwardOnly);
        assert_eq!(service.forward_train_number, 100);
        assert_eq!(service.reverse_train_number, None);
    }

    #[test]
    fn changing_direction_mode_preserves_identity_and_allocates_reverse_number_when_needed() {
        let mut game = game();
        let service_id = create_service_with_mode(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
            ServiceDirectionMode::ForwardOnly,
        )
        .unwrap();

        update_service_with_mode(
            &mut game,
            service_id,
            vec![RailStationId::new(1), RailStationId::new(2)],
            ServiceDirectionMode::BothDirections,
        )
        .unwrap();

        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.name, "R1");
        assert_eq!(service.direction_mode, ServiceDirectionMode::BothDirections);
        assert_eq!(service.forward_train_number, 100);
        assert_eq!(service.reverse_train_number, Some(101));
    }

    #[test]
    fn new_services_receive_unique_consecutive_directional_train_numbers() {
        let mut game = game();

        let first = create_service(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let second = create_service(
            &mut game,
            vec![RailStationId::new(2), RailStationId::new(3)],
        )
        .unwrap();

        let first = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == first)
            .unwrap();
        let second = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == second)
            .unwrap();

        assert_eq!(
            (first.forward_train_number, first.reverse_train_number),
            (100, Some(101))
        );
        assert_eq!(
            (second.forward_train_number, second.reverse_train_number),
            (102, Some(103))
        );
    }

    #[test]
    fn an_unconnected_settlement_is_rejected_without_creating_a_service() {
        let mut game = game();

        assert_eq!(
            find_or_create_service_between_settlements(
                &mut game,
                SettlementId::new(1),
                SettlementId::new(5)
            ),
            Err(ServiceError::UnconnectedSettlement {
                settlement_id: SettlementId::new(5)
            })
        );
        assert!(game.player_company.passenger_services.is_empty());
    }

    #[test]
    fn unused_service_can_be_updated_without_changing_identity() {
        let mut game = game();
        let service_id = create_service(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();

        update_service(
            &mut game,
            service_id,
            vec![RailStationId::new(1), RailStationId::new(3)],
        )
        .unwrap();

        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.name, "R1");
        assert_eq!(
            service.stop_station_ids,
            vec![RailStationId::new(1), RailStationId::new(3)]
        );
        assert_eq!(
            service.rail_line_ids,
            vec![RailLineId::new(1), RailLineId::new(2)]
        );
    }

    #[test]
    fn commercial_name_is_shared_metadata_and_can_be_cleared() {
        let mut game = game();
        let service_id = create_service(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();

        rename_service(&mut game, service_id, Some("  Capital Link  ".into())).unwrap();
        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.custom_name.as_deref(), Some("Capital Link"));
        assert_eq!(service.display_name(), "R1 · Capital Link");

        rename_service(&mut game, service_id, None).unwrap();
        let service = game
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.custom_name, None);
        assert_eq!(service.display_name(), "R1");
    }

    #[test]
    fn ready_train_assignment_can_be_changed_without_dispatching() {
        let mut game = game();
        game.player_company.funds = Money::from_cents(1_000_000);
        let train_id = purchase_train(&mut game, 0, RailStationId::new(1)).unwrap();
        let first_service_id = create_service(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let second_service_id = create_service(
            &mut game,
            vec![RailStationId::new(2), RailStationId::new(3)],
        )
        .unwrap();

        assign_train_to_service(&mut game, train_id, first_service_id).unwrap();
        assert_eq!(
            game.player_company.fleet.assigned_service_id(train_id),
            Some(first_service_id)
        );
        assert_eq!(
            game.player_company.fleet.trains[0].status,
            TrainStatus::Ready {
                at: RailStationId::new(1)
            }
        );

        assign_train_to_service(&mut game, train_id, second_service_id).unwrap();
        assert_eq!(
            game.player_company.fleet.assigned_service_id(train_id),
            Some(second_service_id)
        );

        unassign_train_from_service(&mut game, train_id).unwrap();
        assert_eq!(game.player_company.fleet.assigned_service_id(train_id), None);
    }

    #[test]
    fn travelling_train_assignment_cannot_change_until_arrival() {
        let mut game = game();
        game.player_company.funds = Money::from_cents(1_000_000);
        let train_id = purchase_train(&mut game, 0, RailStationId::new(1)).unwrap();
        let first_service_id = create_service(
            &mut game,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let second_service_id = create_service(
            &mut game,
            vec![RailStationId::new(2), RailStationId::new(3)],
        )
        .unwrap();
        assign_train_to_service(&mut game, train_id, first_service_id).unwrap();
        let journey_id = JourneyId::new(77);
        game.player_company.fleet.trains[0].status = TrainStatus::Travelling { journey_id };

        assert_eq!(
            assign_train_to_service(&mut game, train_id, second_service_id),
            Err(ServiceAssignmentError::TrainTravelling {
                train_id,
                journey_id
            })
        );
        assert_eq!(
            unassign_train_from_service(&mut game, train_id),
            Err(ServiceAssignmentError::TrainTravelling {
                train_id,
                journey_id
            })
        );
        assert_eq!(
            assign_train_to_service(&mut game, train_id, first_service_id),
            Ok(())
        );
        assert_eq!(
            game.player_company.fleet.assigned_service_id(train_id),
            Some(first_service_id)
        );
    }

    #[test]
    fn unused_service_can_be_deleted_and_releases_assigned_trains() {
        let mut game = game();
        game.player_company.funds = Money::from_cents(1_000_000);
        let train_id = purchase_train(&mut game, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        assign_train_to_service(&mut game, train_id, service_id).unwrap();

        delete_service(&mut game, service_id).unwrap();

        assert!(game.player_company.passenger_services.is_empty());
        assert_eq!(game.player_company.fleet.assigned_service_id(train_id), None);
    }
}

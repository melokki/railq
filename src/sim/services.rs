//! Passenger Service path lookup, creation, editing, and deletion.
//!
//! Services belong to the Player Company. They reuse Rail Authority-owned
//! Rail Lines, remain directional, and are deliberately distinct from
//! individual Journeys.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
};

use crate::model::{
    GameState, PassengerService, RailLineId, RailNetwork, RailStationId, ServiceId, SettlementId,
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
        }
    }
}

impl Error for ServiceError {}

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

/// Creates one named, directional Passenger Service.
///
/// Names are intentionally generated from the persistent Service ID for now;
/// a later naming feature can change the display name without changing identity.
pub fn create_service(
    state: &mut GameState,
    stop_station_ids: Vec<RailStationId>,
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
    state
        .player_company
        .passenger_services
        .push(PassengerService {
            id: service_id,
            name: service_name,
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

    let service = &mut state.player_company.passenger_services[index];
    service.stop_station_ids = stop_station_ids;
    service.rail_line_ids = rail_line_ids;
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
        model::{RailLineId, RailStationId, SettlementId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{
        ServiceError, create_service, delete_service, find_or_create_service,
        find_or_create_service_between_settlements, path_between_stations, service_path_for_stops,
        update_service,
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
    fn unused_service_can_be_deleted() {
        let mut game = game();
        let service_id =
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(2))
                .unwrap();

        delete_service(&mut game, service_id).unwrap();

        assert!(game.player_company.passenger_services.is_empty());
    }
}

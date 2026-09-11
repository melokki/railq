//! Passenger Service path lookup and creation.
//!
//! Services belong to the Player Company. They reuse Rail Authority-owned
//! Rail Lines, and are deliberately distinct from individual Journeys.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
};

use crate::model::{
    GameState, PassengerService, RailLineId, RailNetwork, RailStationId, ServiceId, SettlementId,
};

/// Why a Passenger Service path cannot be selected or created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceError {
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
    /// A new Service ID cannot be represented.
    ServiceIdExhausted,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SameEndpoint { station_id } => {
                write!(
                    formatter,
                    "Rail Station {} cannot be both Service endpoints",
                    station_id.get()
                )
            }
            Self::RailStationNotFound { station_id } => {
                write!(
                    formatter,
                    "Rail Station {} is not in the Rail Network",
                    station_id.get()
                )
            }
            Self::SettlementNotFound { settlement_id } => {
                write!(
                    formatter,
                    "Settlement {} is not in the Region",
                    settlement_id.get()
                )
            }
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

/// Finds or creates the Passenger Service between two connected Rail Stations.
///
/// Endpoint order does not create a second Service: a Passenger Service is
/// usable in either direction. New Service creation is free.
pub fn find_or_create_service(
    state: &mut GameState,
    first_station_id: RailStationId,
    second_station_id: RailStationId,
) -> Result<ServiceId, ServiceError> {
    if first_station_id == second_station_id {
        return Err(ServiceError::SameEndpoint {
            station_id: first_station_id,
        });
    }

    if let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| {
            (service.first_station_id == first_station_id
                && service.second_station_id == second_station_id)
                || (service.first_station_id == second_station_id
                    && service.second_station_id == first_station_id)
        })
    {
        return Ok(service.id);
    }

    let rail_line_ids = path_between_stations(
        &state.region.rail_authority.rail_network,
        first_station_id,
        second_station_id,
    )?;
    let service_id = next_service_id(&state.player_company.passenger_services)?;
    state
        .player_company
        .passenger_services
        .push(PassengerService {
            id: service_id,
            first_station_id,
            second_station_id,
            rail_line_ids,
        });
    Ok(service_id)
}

/// Resolves selected Settlements to Rail Stations before creating or reusing a
/// Passenger Service. This rejects the Region's unconnected Settlements.
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

fn next_service_id(passenger_services: &[PassengerService]) -> Result<ServiceId, ServiceError> {
    passenger_services
        .iter()
        .map(|service| service.id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .map(ServiceId::new)
        .ok_or(ServiceError::ServiceIdExhausted)
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{RailLineId, RailStationId, SettlementId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{
        ServiceError, find_or_create_service, find_or_create_service_between_settlements,
        path_between_stations,
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
    fn selecting_the_same_endpoint_is_rejected_without_creating_a_service() {
        let mut game = game();

        assert_eq!(
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(1)),
            Err(ServiceError::SameEndpoint {
                station_id: RailStationId::new(1)
            })
        );
        assert!(game.player_company.passenger_services.is_empty());
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
    fn repeated_endpoint_selection_reuses_the_existing_service_in_both_directions() {
        let mut game = game();

        let forward =
            find_or_create_service(&mut game, RailStationId::new(1), RailStationId::new(3))
                .unwrap();
        let reverse =
            find_or_create_service(&mut game, RailStationId::new(3), RailStationId::new(1))
                .unwrap();

        assert_eq!(forward, reverse);
        assert_eq!(game.player_company.passenger_services.len(), 1);
        assert_eq!(
            game.player_company.passenger_services[0].rail_line_ids,
            vec![RailLineId::new(1), RailLineId::new(2)]
        );
    }
}

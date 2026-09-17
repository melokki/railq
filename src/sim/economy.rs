//! Pure Journey economy calculations.
//!
//! A quote observes the current Player Company, Passenger Service, Rail
//! Network, and directional Passenger Demand without reserving passengers or
//! changing Company Funds. Manual dispatch applies the quoted effects later.

use std::{error::Error, fmt};

use crate::{
    catalog::model_for_train,
    model::{
        CalculationError, DistanceMetres, DurationSeconds, GameState, InfrastructureProjectId,
        InfrastructureProjectStatus, Money, MoneyPerKilometre, PassengerService, RailLineId,
        RailStationId, ServiceDirectionMode, ServiceId, SpeedMetresPerSecond, TrainId, TrainStatus,
    },
    sim::services::path_between_stations,
};

/// One group of passengers that would board at one Service stop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PassengerBoardingQuote {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub passengers: u32,
    pub fare: Money,
    pub revenue: Money,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InfrastructureAccessCreditUse {
    pub project_id: InfrastructureProjectId,
    pub amount: Money,
}

/// The current economic and operational terms for one possible Journey.
///
/// Operating costs cover the complete Service run and are paid at initial
/// dispatch. `operating_revenue` contains only revenue from passengers that can
/// board at the origin now; later Service stops may add more passengers and
/// revenue while the same Journey remains active.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JourneyQuote {
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub rail_line_path: Vec<RailLineId>,
    pub distance: DistanceMetres,
    pub boarded_passengers: u32,
    /// Through fare from Service origin to terminus.
    pub fare: Money,
    pub operating_revenue: Money,
    pub infrastructure_access_fee_before_credit: Money,
    pub infrastructure_access_fee_credit: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
    pub operating_cost: Money,
    pub journey_profitability: Money,
    /// Total duration of the complete Service path at this Train's speed.
    pub duration: DurationSeconds,
    /// Duration to the first Service stop after the origin.
    pub first_leg_duration: DurationSeconds,
    pub cash_after_cost: Money,
    pub boarding_groups: Vec<PassengerBoardingQuote>,
    pub(crate) access_fee_credit_uses: Vec<InfrastructureAccessCreditUse>,
}

/// The current economic and operational terms for one empty positioning move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositioningQuote {
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub rail_line_path: Vec<RailLineId>,
    pub distance: DistanceMetres,
    pub infrastructure_access_fee_before_credit: Money,
    pub infrastructure_access_fee_credit: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
    pub operating_cost: Money,
    pub duration: DurationSeconds,
    pub cash_after_cost: Money,
    pub(crate) access_fee_credit_uses: Vec<InfrastructureAccessCreditUse>,
}

/// Why a Journey cannot be quoted from the current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EconomyError {
    TrainNotFound {
        train_id: TrainId,
    },
    TrainModelNotFound {
        train_id: TrainId,
    },
    ServiceNotFound {
        service_id: ServiceId,
    },
    TrainTravelling {
        train_id: TrainId,
    },
    TrainNotAssignedToService {
        train_id: TrainId,
        service_id: ServiceId,
    },
    TrainAssignedToOtherService {
        train_id: TrainId,
        assigned_service_id: ServiceId,
        requested_service_id: ServiceId,
    },
    TrainNotAtServiceOrigin {
        train_id: TrainId,
        station_id: RailStationId,
        service_id: ServiceId,
    },
    InvalidPositioningDestination {
        service_id: ServiceId,
        station_id: RailStationId,
    },
    TrainAlreadyAtPositioningDestination {
        train_id: TrainId,
        station_id: RailStationId,
    },
    PositioningPathNotFound {
        origin_station_id: RailStationId,
        destination_station_id: RailStationId,
    },
    EmptyServicePath {
        service_id: ServiceId,
    },
    RailLineNotFound {
        rail_line_id: RailLineId,
    },
    InvalidServiceStops {
        service_id: ServiceId,
    },
    DirectionalDemandNotFound {
        origin_station_id: RailStationId,
        destination_station_id: RailStationId,
    },
    Calculation(CalculationError),
}

impl fmt::Display for EconomyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TrainNotFound { train_id } => {
                write!(formatter, "Train {} is not in the Fleet", train_id.get())
            }
            Self::TrainModelNotFound { train_id } => write!(
                formatter,
                "Train {} references a model absent from the Train catalogue",
                train_id.get()
            ),
            Self::ServiceNotFound { service_id } => write!(
                formatter,
                "Passenger Service {} is not owned by the Player Company",
                service_id.get()
            ),
            Self::TrainTravelling { train_id } => write!(
                formatter,
                "Train {} is already travelling and cannot be quoted",
                train_id.get()
            ),
            Self::TrainNotAssignedToService {
                train_id,
                service_id,
            } => write!(
                formatter,
                "Train {} is not assigned to Passenger Service {}",
                train_id.get(),
                service_id.get()
            ),
            Self::TrainAssignedToOtherService {
                train_id,
                assigned_service_id,
                requested_service_id,
            } => write!(
                formatter,
                "Train {} is assigned to Passenger Service {} and cannot operate Passenger Service {}",
                train_id.get(),
                assigned_service_id.get(),
                requested_service_id.get()
            ),
            Self::TrainNotAtServiceOrigin {
                train_id,
                station_id,
                service_id,
            } => write!(
                formatter,
                "Train {} at Rail Station {} is not at the origin of directional Passenger Service {}",
                train_id.get(),
                station_id.get(),
                service_id.get()
            ),
            Self::InvalidPositioningDestination {
                service_id,
                station_id,
            } => write!(
                formatter,
                "Rail Station {} is not a valid departure terminus for Passenger Service {}",
                station_id.get(),
                service_id.get()
            ),
            Self::TrainAlreadyAtPositioningDestination {
                train_id,
                station_id,
            } => write!(
                formatter,
                "Train {} is already at Rail Station {}",
                train_id.get(),
                station_id.get()
            ),
            Self::PositioningPathNotFound {
                origin_station_id,
                destination_station_id,
            } => write!(
                formatter,
                "no open Rail Line path exists from Rail Station {} to Rail Station {}",
                origin_station_id.get(),
                destination_station_id.get()
            ),
            Self::EmptyServicePath { service_id } => write!(
                formatter,
                "Passenger Service {} has no Rail Line path",
                service_id.get()
            ),
            Self::RailLineNotFound { rail_line_id } => write!(
                formatter,
                "Rail Line {} is not in the Rail Network",
                rail_line_id.get()
            ),
            Self::InvalidServiceStops { service_id } => write!(
                formatter,
                "Passenger Service {} does not contain a valid ordered stop pattern",
                service_id.get()
            ),
            Self::DirectionalDemandNotFound {
                origin_station_id,
                destination_station_id,
            } => write!(
                formatter,
                "no directional Passenger Demand exists from Rail Station {} to Rail Station {}",
                origin_station_id.get(),
                destination_station_id.get()
            ),
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for EconomyError {}

impl From<CalculationError> for EconomyError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

/// Calculates a Journey quote without changing game state.
///
/// Passengers at the Service origin may board for any later Service stop, not
/// only the terminus. Seats are filled deterministically in stop order, so
/// nearer destinations board first and can free capacity at intermediate stops.
pub fn quote_journey(
    state: &GameState,
    train_id: TrainId,
    service_id: ServiceId,
) -> Result<JourneyQuote, EconomyError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or(EconomyError::TrainNotFound { train_id })?;
    let train_model =
        model_for_train(train).ok_or(EconomyError::TrainModelNotFound { train_id })?;
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .ok_or(EconomyError::ServiceNotFound { service_id })?;
    if let Some(assigned_service_id) = state.player_company.fleet.assigned_service_id(train_id)
        && assigned_service_id != service_id
    {
        return Err(EconomyError::TrainAssignedToOtherService {
            train_id,
            assigned_service_id,
            requested_service_id: service_id,
        });
    }
    let origin_station_id = match train.status {
        TrainStatus::Ready { at } => at,
        TrainStatus::Travelling { .. } => return Err(EconomyError::TrainTravelling { train_id }),
    };
    if service.stop_station_ids.len() < 2 || service.rail_line_ids.is_empty() {
        return Err(EconomyError::EmptyServicePath { service_id });
    }
    let last_stop_index = service.stop_station_ids.len() - 1;
    let (origin_stop_index, destination_stop_index, direction) =
        if origin_station_id == service.stop_station_ids[0] {
            (0, last_stop_index, 1)
        } else if origin_station_id == service.stop_station_ids[last_stop_index]
            && service.direction_mode == ServiceDirectionMode::BothDirections
        {
            (last_stop_index, 0, -1)
        } else {
            return Err(EconomyError::TrainNotAtServiceOrigin {
                train_id,
                station_id: origin_station_id,
                service_id,
            });
        };
    let service_destination = service.stop_station_ids[destination_stop_index];

    let distance = distance_for_lines(state, &service.rail_line_ids)?;
    let fare_rate = state.rules.balance.fare_per_passenger_kilometre();
    let boarding_groups = quote_boarding_at_stop_in_direction(
        state,
        service,
        origin_stop_index,
        direction,
        train_model.passenger_capacity().passengers(),
        fare_rate,
    )?;
    let boarded_passengers = boarding_groups.iter().try_fold(0_u32, |total, group| {
        total
            .checked_add(group.passengers)
            .ok_or(CalculationError::Overflow {
                operation: "Journey boarded passengers",
            })
            .map_err(EconomyError::from)
    })?;
    let operating_revenue = boarding_groups
        .iter()
        .try_fold(Money::ZERO, |total, group| {
            total.checked_add(group.revenue).map_err(EconomyError::from)
        })?;
    let fare = fare_rate.checked_charge(distance)?;
    let access_fee_rate = state.rules.balance.access_fee_per_train_kilometre();
    let infrastructure_access_fee_before_credit = access_fee_rate.checked_charge(distance)?;
    let (infrastructure_access_fee_credit, access_fee_credit_uses) =
        quote_infrastructure_access_credits(
            state,
            &service.rail_line_ids,
            access_fee_rate,
            infrastructure_access_fee_before_credit,
        )?;
    let infrastructure_access_fee =
        infrastructure_access_fee_before_credit.checked_sub(infrastructure_access_fee_credit)?;
    let fuel_cost = train_model
        .fuel_cost_per_kilometre()
        .checked_charge(distance)?;
    let operating_cost = infrastructure_access_fee.checked_add(fuel_cost)?;
    let journey_profitability = operating_revenue.checked_sub(operating_cost)?;
    let duration = service_duration(state, service, train_model.speed())?;
    let first_leg_stop_index = if direction > 0 {
        origin_stop_index + 1
    } else {
        origin_stop_index - 1
    };
    let first_leg_duration = duration_between_service_stops(
        state,
        service,
        origin_stop_index,
        first_leg_stop_index,
        train_model.speed(),
    )?;
    let cash_after_cost = state.player_company.funds.checked_sub(operating_cost)?;
    let rail_line_path = if direction > 0 {
        service.rail_line_ids.clone()
    } else {
        service.rail_line_ids.iter().rev().copied().collect()
    };

    Ok(JourneyQuote {
        service_id,
        train_id,
        origin_station_id,
        destination_station_id: service_destination,
        rail_line_path,
        distance,
        boarded_passengers,
        fare,
        operating_revenue,
        infrastructure_access_fee_before_credit,
        infrastructure_access_fee_credit,
        infrastructure_access_fee,
        fuel_cost,
        operating_cost,
        journey_profitability,
        duration,
        first_leg_duration,
        cash_after_cost,
        boarding_groups,
        access_fee_credit_uses,
    })
}

/// Quotes an empty movement that positions an assigned READY Train at a
/// departure terminus of its Passenger Service. No passenger demand or fare
/// revenue is involved; infrastructure and fuel costs are paid at departure.
pub fn quote_positioning_journey(
    state: &GameState,
    train_id: TrainId,
    service_id: ServiceId,
    destination_station_id: RailStationId,
) -> Result<PositioningQuote, EconomyError> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or(EconomyError::TrainNotFound { train_id })?;
    let train_model =
        model_for_train(train).ok_or(EconomyError::TrainModelNotFound { train_id })?;
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
        .ok_or(EconomyError::ServiceNotFound { service_id })?;

    match state.player_company.fleet.assigned_service_id(train_id) {
        None => {
            return Err(EconomyError::TrainNotAssignedToService {
                train_id,
                service_id,
            });
        }
        Some(assigned_service_id) if assigned_service_id != service_id => {
            return Err(EconomyError::TrainAssignedToOtherService {
                train_id,
                assigned_service_id,
                requested_service_id: service_id,
            });
        }
        Some(_) => {}
    }

    let origin_station_id = match train.status {
        TrainStatus::Ready { at } => at,
        TrainStatus::Travelling { .. } => return Err(EconomyError::TrainTravelling { train_id }),
    };
    if !service.accepts_departure_station(destination_station_id) {
        return Err(EconomyError::InvalidPositioningDestination {
            service_id,
            station_id: destination_station_id,
        });
    }
    if origin_station_id == destination_station_id {
        return Err(EconomyError::TrainAlreadyAtPositioningDestination {
            train_id,
            station_id: destination_station_id,
        });
    }

    let rail_line_path = path_between_stations(
        &state.region.rail_authority.rail_network,
        origin_station_id,
        destination_station_id,
    )
    .map_err(|_| EconomyError::PositioningPathNotFound {
        origin_station_id,
        destination_station_id,
    })?;
    let distance = distance_for_lines(state, &rail_line_path)?;
    let access_fee_rate = state.rules.balance.access_fee_per_train_kilometre();
    let infrastructure_access_fee_before_credit = access_fee_rate.checked_charge(distance)?;
    let (infrastructure_access_fee_credit, access_fee_credit_uses) =
        quote_infrastructure_access_credits(
            state,
            &rail_line_path,
            access_fee_rate,
            infrastructure_access_fee_before_credit,
        )?;
    let infrastructure_access_fee =
        infrastructure_access_fee_before_credit.checked_sub(infrastructure_access_fee_credit)?;
    let fuel_cost = train_model
        .fuel_cost_per_kilometre()
        .checked_charge(distance)?;
    let operating_cost = infrastructure_access_fee.checked_add(fuel_cost)?;
    let duration = duration_for_lines(state, &rail_line_path, train_model.speed())?;
    let cash_after_cost = state.player_company.funds.checked_sub(operating_cost)?;

    Ok(PositioningQuote {
        service_id,
        train_id,
        origin_station_id,
        destination_station_id,
        rail_line_path,
        distance,
        infrastructure_access_fee_before_credit,
        infrastructure_access_fee_credit,
        infrastructure_access_fee,
        fuel_cost,
        operating_cost,
        duration,
        cash_after_cost,
        access_fee_credit_uses,
    })
}

fn quote_infrastructure_access_credits(
    state: &GameState,
    rail_line_ids: &[RailLineId],
    rate: MoneyPerKilometre,
    gross_fee: Money,
) -> Result<(Money, Vec<InfrastructureAccessCreditUse>), EconomyError> {
    let mut uses: Vec<InfrastructureAccessCreditUse> = Vec::new();
    let mut total_credit = Money::ZERO;

    for rail_line_id in rail_line_ids {
        let line = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .find(|line| line.id == *rail_line_id)
            .ok_or(EconomyError::RailLineNotFound {
                rail_line_id: *rail_line_id,
            })?;
        let mut eligible_fee = rate.checked_charge(line.distance)?;

        for project in &state.region.rail_authority.infrastructure_projects {
            if eligible_fee <= Money::ZERO {
                break;
            }
            if project.status != InfrastructureProjectStatus::Open
                || !project.access_credit_covers_line(*rail_line_id)
                || project.funding.access_fee_credit_remaining <= Money::ZERO
            {
                continue;
            }

            let already_reserved = uses
                .iter()
                .filter(|usage| usage.project_id == project.id)
                .try_fold(Money::ZERO, |total, usage| total.checked_add(usage.amount))?;
            let available = project
                .funding
                .access_fee_credit_remaining
                .checked_sub(already_reserved)?;
            if available <= Money::ZERO {
                continue;
            }

            let remaining_gross = gross_fee.checked_sub(total_credit)?;
            if remaining_gross <= Money::ZERO {
                break;
            }
            let applied = available.min(eligible_fee).min(remaining_gross);
            if applied <= Money::ZERO {
                continue;
            }

            if let Some(existing) = uses.iter_mut().find(|usage| usage.project_id == project.id) {
                existing.amount = existing.amount.checked_add(applied)?;
            } else {
                uses.push(InfrastructureAccessCreditUse {
                    project_id: project.id,
                    amount: applied,
                });
            }
            total_credit = total_credit.checked_add(applied)?;
            eligible_fee = eligible_fee.checked_sub(applied)?;
        }
    }

    Ok((total_credit, uses))
}

/// Calculates boardings from one stop in the requested Service direction.
///
/// `direction` is `1` for the canonical first-to-last working and `-1` for
/// the reverse working of a bidirectional Service.
pub(crate) fn quote_boarding_at_stop_in_direction(
    state: &GameState,
    service: &PassengerService,
    stop_index: usize,
    direction: i32,
    available_capacity: u32,
    fare_rate: MoneyPerKilometre,
) -> Result<Vec<PassengerBoardingQuote>, EconomyError> {
    let destination_indices: Box<dyn Iterator<Item = usize>> = match direction {
        1 => Box::new((stop_index + 1)..service.stop_station_ids.len()),
        -1 => Box::new((0..stop_index).rev()),
        _ => {
            return Err(EconomyError::InvalidServiceStops {
                service_id: service.id,
            });
        }
    };
    let Some(&origin_station_id) = service.stop_station_ids.get(stop_index) else {
        return Err(EconomyError::InvalidServiceStops {
            service_id: service.id,
        });
    };
    let mut remaining_capacity = available_capacity;
    let mut groups = Vec::new();

    for destination_index in destination_indices {
        if remaining_capacity == 0 {
            break;
        }
        let destination_station_id = service.stop_station_ids[destination_index];
        let demand = state
            .origin_destination_demand
            .iter()
            .find(|demand| {
                demand.origin_station_id == origin_station_id
                    && demand.destination_station_id == destination_station_id
            })
            .ok_or(EconomyError::DirectionalDemandNotFound {
                origin_station_id,
                destination_station_id,
            })?;
        let passengers = demand.waiting_passengers.min(remaining_capacity);
        if passengers == 0 {
            continue;
        }
        let distance =
            distance_between_service_stops(state, service, stop_index, destination_index)?;
        let fare = fare_rate.checked_charge(distance)?;
        let revenue = fare.checked_mul(u64::from(passengers))?;
        groups.push(PassengerBoardingQuote {
            origin_station_id,
            destination_station_id,
            passengers,
            fare,
            revenue,
        });
        remaining_capacity -= passengers;
    }

    Ok(groups)
}

fn service_duration(
    state: &GameState,
    service: &PassengerService,
    train_speed: SpeedMetresPerSecond,
) -> Result<DurationSeconds, EconomyError> {
    duration_for_lines(state, &service.rail_line_ids, train_speed)
}

pub(crate) fn duration_between_service_stops(
    state: &GameState,
    service: &PassengerService,
    first_stop_index: usize,
    second_stop_index: usize,
    train_speed: SpeedMetresPerSecond,
) -> Result<DurationSeconds, EconomyError> {
    let line_ids =
        line_ids_between_service_stops(state, service, first_stop_index, second_stop_index)?;
    duration_for_lines(state, &line_ids, train_speed)
}

pub(crate) fn distance_between_service_stops(
    state: &GameState,
    service: &PassengerService,
    first_stop_index: usize,
    second_stop_index: usize,
) -> Result<DistanceMetres, EconomyError> {
    let line_ids =
        line_ids_between_service_stops(state, service, first_stop_index, second_stop_index)?;
    distance_for_lines(state, &line_ids)
}

fn line_ids_between_service_stops(
    state: &GameState,
    service: &PassengerService,
    first_stop_index: usize,
    second_stop_index: usize,
) -> Result<Vec<RailLineId>, EconomyError> {
    let Some(&first_station_id) = service.stop_station_ids.get(first_stop_index) else {
        return Err(EconomyError::InvalidServiceStops {
            service_id: service.id,
        });
    };
    let Some(&second_station_id) = service.stop_station_ids.get(second_stop_index) else {
        return Err(EconomyError::InvalidServiceStops {
            service_id: service.id,
        });
    };
    path_between_stations(
        &state.region.rail_authority.rail_network,
        first_station_id,
        second_station_id,
    )
    .map_err(|_| EconomyError::InvalidServiceStops {
        service_id: service.id,
    })
}

fn duration_for_lines(
    state: &GameState,
    rail_line_ids: &[RailLineId],
    train_speed: SpeedMetresPerSecond,
) -> Result<DurationSeconds, EconomyError> {
    let mut seconds = 0_u64;
    for rail_line_id in rail_line_ids {
        let rail_line = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .find(|line| line.id == *rail_line_id)
            .ok_or(EconomyError::RailLineNotFound {
                rail_line_id: *rail_line_id,
            })?;
        let segment_seconds = rail_line
            .distance
            .journey_duration_with_speed_limit(train_speed, rail_line.speed_limit)?
            .seconds();
        seconds = seconds
            .checked_add(segment_seconds)
            .ok_or(CalculationError::Overflow {
                operation: "Service journey duration",
            })?;
    }
    Ok(DurationSeconds::from_seconds(seconds))
}

fn distance_for_lines(
    state: &GameState,
    rail_line_ids: &[RailLineId],
) -> Result<DistanceMetres, EconomyError> {
    let distance_metres = rail_line_ids
        .iter()
        .try_fold(0_u64, |total, rail_line_id| {
            let rail_line = state
                .region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .find(|line| line.id == *rail_line_id)
                .ok_or(EconomyError::RailLineNotFound {
                    rail_line_id: *rail_line_id,
                })?;
            total
                .checked_add(rail_line.distance.metres())
                .ok_or(CalculationError::Overflow {
                    operation: "Journey path distance",
                })
                .map_err(EconomyError::from)
        })?;
    let metres = i64::try_from(distance_metres).map_err(|_| CalculationError::Overflow {
        operation: "Journey path distance",
    })?;
    DistanceMetres::new(metres)
        .map_err(|_| CalculationError::Overflow {
            operation: "Journey path distance",
        })
        .map_err(EconomyError::from)
}

#[cfg(test)]
mod tests {
    use crate::{
        balance::BalanceConfig,
        model::{
            AuthorityRules, DemandRules, Financials, Fleet, GameRules, MarketMaturity,
            OriginDestinationDemand, PassengerArrivalRate, PassengerService, PlayerCompany,
            RailAuthority, RailLine, RailNetwork, RailStation, Settlement, Train, UtcSeconds,
        },
    };

    use super::*;

    const SERVICE_ID: ServiceId = ServiceId::new(1);
    const TRAIN_ID: TrainId = TrainId::new(1);
    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(3);
    const FIRST_LINE: RailLineId = RailLineId::new(1);
    const SECOND_LINE: RailLineId = RailLineId::new(2);

    fn fixture() -> GameState {
        let fare_rate = crate::model::MoneyPerKilometre::new(11).unwrap();
        let access_rate = crate::model::MoneyPerKilometre::new(5).unwrap();
        GameState {
            world_seed: 0,
            region: crate::model::Region {
                name: "Fixture Region".into(),
                railway_registration: crate::model::RailwayRegistration {
                    numeric_code: 99,
                    mark: "RQ".into(),
                },
                population: 3,
                settlements: vec![
                    Settlement {
                        id: crate::model::SettlementId::new(1),
                        name: "Origin".into(),
                        population: 1,
                        position: crate::model::WorldPosition::default(),
                    },
                    Settlement {
                        id: crate::model::SettlementId::new(2),
                        name: "Middle".into(),
                        population: 1,
                        position: crate::model::WorldPosition::default(),
                    },
                    Settlement {
                        id: crate::model::SettlementId::new(3),
                        name: "Destination".into(),
                        population: 1,
                        position: crate::model::WorldPosition::default(),
                    },
                ],
                bulletin: vec![],
                rail_authority: RailAuthority {
                    name: "Fixture Rail Authority".into(),
                    rail_network: RailNetwork {
                        rail_stations: vec![
                            RailStation {
                                id: ORIGIN,
                                settlement_id: crate::model::SettlementId::new(1),
                            },
                            RailStation {
                                id: RailStationId::new(2),
                                settlement_id: crate::model::SettlementId::new(2),
                            },
                            RailStation {
                                id: DESTINATION,
                                settlement_id: crate::model::SettlementId::new(3),
                            },
                        ],
                        rail_lines: vec![
                            RailLine {
                                id: FIRST_LINE,
                                first_station_id: ORIGIN,
                                second_station_id: RailStationId::new(2),
                                distance: DistanceMetres::new(1_000).unwrap(),
                                speed_limit: crate::model::SpeedKilometresPerHour::new(70).unwrap(),
                                track_count: crate::model::TrackCount::SINGLE,
                                electrification: crate::model::Electrification::None,
                                construction_difficulty:
                                    crate::model::ConstructionDifficulty::Moderate,
                            },
                            RailLine {
                                id: SECOND_LINE,
                                first_station_id: RailStationId::new(2),
                                second_station_id: DESTINATION,
                                distance: DistanceMetres::new(501).unwrap(),
                                speed_limit: crate::model::SpeedKilometresPerHour::new(70).unwrap(),
                                track_count: crate::model::TrackCount::SINGLE,
                                electrification: crate::model::Electrification::None,
                                construction_difficulty:
                                    crate::model::ConstructionDifficulty::Moderate,
                            },
                        ],
                    },
                    finances: crate::model::RailAuthorityFinances::default(),
                    construction_capacity: crate::model::PROVISIONAL_CONSTRUCTION_CAPACITY,
                    infrastructure_projects: vec![],
                },
            },
            player_company: PlayerCompany {
                name: "Fixture Passenger".into(),
                vehicle_keeper_mark: crate::model::VehicleKeeperMark::generated_from_company_name(
                    "Fixture Passenger",
                ),
                funds: Money::from_cents(10_000),
                fleet: Fleet {
                    service_assignments: Default::default(),
                    next_train_display_number: 2,
                    trains: vec![Train {
                        id: TRAIN_ID,
                        evn: crate::model::EuropeanVehicleNumber::generate(95, 99, 701, 1).unwrap(),
                        nickname: None,
                        status: TrainStatus::Ready { at: ORIGIN },
                        model_id: crate::model::TrainModelId::new("helvetra-r70"),
                        original_purchase_price: Money::from_cents(5_000),
                    }],
                    next_evn_unit_by_model: [(crate::model::TrainModelId::new("helvetra-r70"), 2)]
                        .into_iter()
                        .collect(),
                },
                passenger_services: vec![PassengerService {
                    id: SERVICE_ID,
                    name: "R1".into(),
                    custom_name: None,
                    direction_mode: crate::model::ServiceDirectionMode::BothDirections,
                    forward_train_number: 100,
                    reverse_train_number: Some(101),
                    stop_station_ids: vec![ORIGIN, DESTINATION],
                    rail_line_ids: vec![FIRST_LINE, SECOND_LINE],
                }],
            },
            origin_destination_demand: vec![
                OriginDestinationDemand {
                    origin_station_id: ORIGIN,
                    destination_station_id: DESTINATION,
                    waiting_passengers: 3,
                    market_maturity: MarketMaturity::full(),
                    passenger_arrival_rate_per_hour: PassengerArrivalRate::new(1).unwrap(),
                    fractional_passenger_seconds: 0,
                },
                OriginDestinationDemand {
                    origin_station_id: DESTINATION,
                    destination_station_id: ORIGIN,
                    waiting_passengers: 1,
                    market_maturity: MarketMaturity::full(),
                    passenger_arrival_rate_per_hour: PassengerArrivalRate::new(1).unwrap(),
                    fractional_passenger_seconds: 0,
                },
            ],
            active_journeys: vec![],
            financials: Financials {
                operating_revenue: Money::ZERO,
                infrastructure_access_fees: Money::ZERO,
                fuel_costs: Money::ZERO,
                recent_journey_receipts: vec![],
            },
            rules: GameRules {
                balance: BalanceConfig::new(fare_rate, access_rate, Money::from_cents(10_000)),
                demand: DemandRules::provisional(),
                authority: AuthorityRules::provisional(),
            },
            last_processed_at: UtcSeconds::from_unix_seconds(0),
        }
    }

    #[test]
    fn known_fixture_independently_verifies_journey_economics_and_duration() {
        let quote = quote_journey(&fixture(), TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(quote.rail_line_path, [FIRST_LINE, SECOND_LINE]);
        assert_eq!(quote.distance, DistanceMetres::new(1_501).unwrap());
        assert_eq!(quote.boarded_passengers, 3);
        assert_eq!(quote.fare, Money::from_cents(17));
        assert_eq!(quote.operating_revenue, Money::from_cents(51));
        assert_eq!(quote.infrastructure_access_fee, Money::from_cents(8));
        assert_eq!(quote.fuel_cost, Money::from_cents(58));
        assert_eq!(quote.operating_cost, Money::from_cents(66));
        assert_eq!(quote.journey_profitability, Money::from_cents(-15));
        assert_eq!(quote.duration, DurationSeconds::from_seconds(78));
        assert_eq!(quote.first_leg_duration, DurationSeconds::from_seconds(78));
        assert_eq!(quote.cash_after_cost, Money::from_cents(9_934));
    }

    #[test]
    fn journey_duration_uses_each_rail_lines_speed_limit() {
        let mut state = fixture();
        state.player_company.passenger_services[0].stop_station_ids =
            vec![ORIGIN, RailStationId::new(2), DESTINATION];
        state.region.rail_authority.rail_network.rail_lines[0].speed_limit =
            crate::model::SpeedKilometresPerHour::new(160).unwrap();
        state.region.rail_authority.rail_network.rail_lines[1].speed_limit =
            crate::model::SpeedKilometresPerHour::new(70).unwrap();

        let quote = quote_journey(&state, TRAIN_ID, SERVICE_ID).unwrap();

        // The 1,000 m first segment is Train-limited: ceil(1000 / 33) = 31 s.
        // The 501 m second segment is line-limited at 70 km/h: 26 s.
        assert_eq!(quote.duration, DurationSeconds::from_seconds(57));
        assert_eq!(quote.first_leg_duration, DurationSeconds::from_seconds(31));
    }

    #[test]
    fn passenger_capacity_limits_boarding() {
        let mut state = fixture();
        state.origin_destination_demand[0].waiting_passengers = 80;

        assert_eq!(
            quote_journey(&state, TRAIN_ID, SERVICE_ID)
                .unwrap()
                .boarded_passengers,
            70
        );
    }

    #[test]
    fn bidirectional_service_quotes_a_reverse_working_from_its_second_terminus() {
        let mut state = fixture();
        state.player_company.fleet.trains[0].status = TrainStatus::Ready { at: DESTINATION };

        let quote = quote_journey(&state, TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(quote.origin_station_id, DESTINATION);
        assert_eq!(quote.destination_station_id, ORIGIN);
        assert_eq!(quote.rail_line_path, [SECOND_LINE, FIRST_LINE]);
        assert_eq!(quote.boarded_passengers, 1);
        assert_eq!(quote.operating_revenue, Money::from_cents(17));
        assert_eq!(quote.first_leg_duration, DurationSeconds::from_seconds(78));
    }

    #[test]
    fn forward_only_service_rejects_a_train_waiting_at_its_destination() {
        let mut state = fixture();
        state.player_company.passenger_services[0].direction_mode =
            ServiceDirectionMode::ForwardOnly;
        state.player_company.passenger_services[0].reverse_train_number = None;
        state.player_company.fleet.trains[0].status = TrainStatus::Ready { at: DESTINATION };

        assert_eq!(
            quote_journey(&state, TRAIN_ID, SERVICE_ID),
            Err(EconomyError::TrainNotAtServiceOrigin {
                train_id: TRAIN_ID,
                station_id: DESTINATION,
                service_id: SERVICE_ID,
            })
        );
    }

    #[test]
    fn forward_only_positioning_accepts_only_the_canonical_origin() {
        let mut state = fixture();
        state.player_company.passenger_services[0].direction_mode =
            ServiceDirectionMode::ForwardOnly;
        state.player_company.passenger_services[0].reverse_train_number = None;
        state
            .player_company
            .fleet
            .service_assignments
            .insert(TRAIN_ID, SERVICE_ID);
        state.player_company.fleet.trains[0].status = TrainStatus::Ready {
            at: RailStationId::new(2),
        };

        assert_eq!(
            quote_positioning_journey(&state, TRAIN_ID, SERVICE_ID, DESTINATION),
            Err(EconomyError::InvalidPositioningDestination {
                service_id: SERVICE_ID,
                station_id: DESTINATION,
            })
        );

        let quote = quote_positioning_journey(&state, TRAIN_ID, SERVICE_ID, ORIGIN)
            .expect("the canonical origin is a valid positioning destination");
        assert_eq!(quote.origin_station_id, RailStationId::new(2));
        assert_eq!(quote.destination_station_id, ORIGIN);
        assert!(quote.operating_cost > Money::ZERO);
    }

    #[test]
    fn positioning_rejects_a_terminus_without_a_constructed_path() {
        let mut state = fixture();
        state
            .player_company
            .fleet
            .service_assignments
            .insert(TRAIN_ID, SERVICE_ID);
        state.player_company.fleet.trains[0].status = TrainStatus::Ready {
            at: RailStationId::new(2),
        };
        state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .retain(|line| line.id != FIRST_LINE);

        assert_eq!(
            quote_positioning_journey(&state, TRAIN_ID, SERVICE_ID, ORIGIN),
            Err(EconomyError::PositioningPathNotFound {
                origin_station_id: RailStationId::new(2),
                destination_station_id: ORIGIN,
            })
        );
    }

    #[test]
    fn assigned_train_cannot_quote_a_different_service() {
        let mut state = fixture();
        let assigned_service_id = ServiceId::new(2);
        state
            .player_company
            .passenger_services
            .push(PassengerService {
                id: assigned_service_id,
                name: "R2".into(),
                custom_name: None,
                direction_mode: ServiceDirectionMode::BothDirections,
                forward_train_number: 102,
                reverse_train_number: Some(103),
                stop_station_ids: vec![ORIGIN, RailStationId::new(2)],
                rail_line_ids: vec![FIRST_LINE],
            });
        state
            .player_company
            .fleet
            .service_assignments
            .insert(TRAIN_ID, assigned_service_id);

        assert_eq!(
            quote_journey(&state, TRAIN_ID, SERVICE_ID),
            Err(EconomyError::TrainAssignedToOtherService {
                train_id: TRAIN_ID,
                assigned_service_id,
                requested_service_id: SERVICE_ID,
            })
        );
    }

    #[test]
    fn quoting_does_not_mutate_game_state() {
        let state = fixture();
        let before = state.clone();

        let _quote = quote_journey(&state, TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(state, before);
    }
    #[test]
    fn open_operator_funded_project_offsets_only_eligible_access_fee() {
        let mut state = fixture();
        state.region.rail_authority.infrastructure_projects.push(
            crate::model::InfrastructureProject {
                id: crate::model::InfrastructureProjectId::new_v4(),
                kind: crate::model::InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: vec![FIRST_LINE],
                    target_speed_limit: crate::model::SpeedKilometresPerHour::new(100).unwrap(),
                },
                status: crate::model::InfrastructureProjectStatus::Open,
                timeline: crate::model::InfrastructureProjectTimeline {
                    requested_at: UtcSeconds::from_unix_seconds(0),
                    review_started_at: None,
                    proposed_at: None,
                    approved_at: None,
                    funding_completed_at: None,
                    scheduled_start_at: None,
                    construction_started_at: None,
                    planned_completion_at: None,
                    completed_at: Some(UtcSeconds::from_unix_seconds(0)),
                    deferred_at: None,
                    cancelled_at: None,
                    reconsideration_count: 0,
                },
                funding: crate::model::InfrastructureProjectFunding {
                    estimated_cost: Money::from_cents(1_000),
                    authority_committed: Money::from_cents(900),
                    operator_contributed: Money::from_cents(100),
                    access_fee_credit_awarded: Money::from_cents(115),
                    access_fee_credit_remaining: Money::from_cents(115),
                },
            },
        );

        let quote = quote_journey(&state, TRAIN_ID, SERVICE_ID).unwrap();

        assert_eq!(
            quote.infrastructure_access_fee_before_credit,
            Money::from_cents(8)
        );
        assert_eq!(quote.infrastructure_access_fee_credit, Money::from_cents(5));
        assert_eq!(quote.infrastructure_access_fee, Money::from_cents(3));
    }
}

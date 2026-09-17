//! Validation of reconstructed or imported game state before it enters the simulation.

use super::*;

/// Why a decoded game cannot safely enter the simulation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveValidationError {
    /// An ID is duplicated within the collection that owns it.
    DuplicateId { kind: &'static str },
    /// An ID is reserved or otherwise not valid for a persisted entity.
    InvalidId { kind: &'static str },
    /// A value is outside the valid domain for a saved game.
    InvalidValue { field: &'static str },
    /// A reference does not resolve within this game state.
    DanglingReference { field: &'static str },
    /// Fields that must agree describe an impossible operating state.
    ImpossibleState { reason: &'static str },
    /// A derived value cannot be calculated in its storage unit.
    Calculation(CalculationError),
}

impl fmt::Display for SaveValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId { kind } => write!(formatter, "duplicate {kind} ID"),
            Self::InvalidId { kind } => write!(formatter, "invalid {kind} ID"),
            Self::InvalidValue { field } => write!(formatter, "invalid saved value for {field}"),
            Self::DanglingReference { field } => {
                write!(
                    formatter,
                    "saved {field} references an entity that does not exist"
                )
            }
            Self::ImpossibleState { reason } => {
                write!(formatter, "impossible saved state: {reason}")
            }
            Self::Calculation(error) => error.fmt(formatter),
        }
    }
}

impl Error for SaveValidationError {}

impl From<CalculationError> for SaveValidationError {
    fn from(error: CalculationError) -> Self {
        Self::Calculation(error)
    }
}

pub fn validate_game_state(state: &GameState) -> Result<(), SaveValidationError> {
    validate_rules(state)?;

    let registration = &state.region.railway_registration;
    if !(10..=99).contains(&registration.numeric_code) {
        return Err(SaveValidationError::InvalidValue {
            field: "Region railway registration code",
        });
    }
    if registration.mark.len() != 2
        || !registration
            .mark
            .chars()
            .all(|character| character.is_ascii_uppercase())
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Region railway registration mark",
        });
    }

    if VehicleKeeperMark::parse(state.player_company.vehicle_keeper_mark.as_str()).is_err() {
        return Err(SaveValidationError::InvalidValue {
            field: "Player Company VKM",
        });
    }

    let settlement_ids = unique_ids(
        state
            .region
            .settlements
            .iter()
            .map(|settlement| settlement.id),
        "Settlement",
    )?;
    if state
        .region
        .settlements
        .iter()
        .any(|settlement| !settlement.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId { kind: "Settlement" });
    }
    let actual_population =
        state
            .region
            .settlements
            .iter()
            .try_fold(0_u64, |total, settlement| {
                total.checked_add(settlement.population).ok_or(
                    SaveValidationError::ImpossibleState {
                        reason: "Region Population overflows its saved unit",
                    },
                )
            })?;
    if state.region.population != actual_population {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Region Population does not equal its Settlement Populations",
        });
    }

    let network = &state.region.rail_authority.rail_network;
    let station_ids = unique_ids(
        network.rail_stations.iter().map(|station| station.id),
        "Rail Station",
    )?;
    if network
        .rail_stations
        .iter()
        .any(|station| !station.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId {
            kind: "Rail Station",
        });
    }
    if network
        .rail_stations
        .iter()
        .any(|station| !settlement_ids.contains(&station.settlement_id))
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Rail Station Settlement",
        });
    }
    let station_settlements = network
        .rail_stations
        .iter()
        .map(|station| station.settlement_id)
        .collect::<HashSet<_>>();
    if station_settlements.len() != network.rail_stations.len() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Settlement has more than one Rail Station",
        });
    }

    let line_ids = unique_ids(network.rail_lines.iter().map(|line| line.id), "Rail Line")?;
    if network.rail_lines.iter().any(|line| !line.id.is_v4()) {
        return Err(SaveValidationError::InvalidId { kind: "Rail Line" });
    }
    for line in &network.rail_lines {
        if !station_ids.contains(&line.first_station_id)
            || !station_ids.contains(&line.second_station_id)
        {
            return Err(SaveValidationError::DanglingReference {
                field: "Rail Line endpoint",
            });
        }
        if line.first_station_id == line.second_station_id {
            return Err(SaveValidationError::ImpossibleState {
                reason: "a Rail Line has the same Rail Station at both endpoints",
            });
        }
    }
    validate_infrastructure_projects(
        &state.region.rail_authority,
        &settlement_ids,
        &station_ids,
        &line_ids,
    )?;
    validate_rail_authority_finances(&state.region.rail_authority)?;

    let mut service_distances = HashMap::new();
    let service_ids = unique_ids(
        state
            .player_company
            .passenger_services
            .iter()
            .map(|service| service.id),
        "Passenger Service",
    )?;
    if state
        .player_company
        .passenger_services
        .iter()
        .any(|service| !service.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId {
            kind: "Passenger Service",
        });
    }
    let mut service_train_numbers = HashSet::new();
    for service in &state.player_company.passenger_services {
        if service.forward_train_number < 100
            || !service_train_numbers.insert(service.forward_train_number)
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Passenger Service train number",
            });
        }
        match (service.direction_mode, service.reverse_train_number) {
            (ServiceDirectionMode::BothDirections, Some(reverse_train_number))
                if reverse_train_number >= 100
                    && service_train_numbers.insert(reverse_train_number) => {}
            (ServiceDirectionMode::ForwardOnly, None) => {}
            _ => {
                return Err(SaveValidationError::InvalidValue {
                    field: "Passenger Service reverse train number",
                });
            }
        }
        if service.name.trim().is_empty() {
            return Err(SaveValidationError::InvalidValue {
                field: "Passenger Service name",
            });
        }
        if let Some(custom_name) = service.custom_name.as_deref() {
            if custom_name.trim().is_empty()
                || custom_name != custom_name.trim()
                || custom_name.chars().count() > PassengerService::MAX_CUSTOM_NAME_CHARACTERS
                || custom_name.chars().any(char::is_control)
            {
                return Err(SaveValidationError::InvalidValue {
                    field: "Passenger Service commercial name",
                });
            }
        }
        if service
            .stop_station_ids
            .iter()
            .any(|station_id| !station_ids.contains(station_id))
        {
            return Err(SaveValidationError::DanglingReference {
                field: "Passenger Service stop",
            });
        }
        let distance = service_distance(service, network, &line_ids)?;
        service_distances.insert(service.id, distance);
    }

    validate_demand(state, &station_ids)?;
    validate_financials(state)?;

    let train_ids = unique_ids(
        state
            .player_company
            .fleet
            .trains
            .iter()
            .map(|train| train.id),
        "Train",
    )?;
    if state
        .player_company
        .fleet
        .trains
        .iter()
        .any(|train| !train.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId { kind: "Train" });
    }
    let highest_train_display_number = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| train.id.get())
        .max()
        .unwrap_or(0);
    if state.player_company.fleet.next_train_display_number == 0
        || state.player_company.fleet.next_train_display_number <= highest_train_display_number
    {
        return Err(SaveValidationError::InvalidValue {
            field: "next Train display number",
        });
    }

    for (model_id, next_unit_number) in &state.player_company.fleet.next_evn_unit_by_model {
        if train_catalogue().by_id(model_id).is_none()
            || *next_unit_number == 0
            || *next_unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER + 1
        {
            return Err(SaveValidationError::InvalidValue {
                field: "next EVN unit number",
            });
        }
    }
    for train in &state.player_company.fleet.trains {
        let Some(next_unit_number) = state
            .player_company
            .fleet
            .next_evn_unit_by_model
            .get(&train.model_id)
        else {
            return Err(SaveValidationError::InvalidValue {
                field: "next EVN unit number",
            });
        };
        if train.evn.unit_number() >= *next_unit_number {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Train EVN unit number has not been consumed by its model sequence",
            });
        }
    }

    let journey_ids = unique_ids(
        state.active_journeys.iter().map(|journey| journey.id),
        "Journey",
    )?;
    if state
        .active_journeys
        .iter()
        .any(|journey| !journey.id.is_v4())
    {
        return Err(SaveValidationError::InvalidId { kind: "Journey" });
    }

    validate_service_assignments(state, &train_ids, &service_ids)?;
    validate_train_statuses(
        &state.player_company.fleet.trains,
        state.region.railway_registration.numeric_code,
        &station_ids,
        &journey_ids,
    )?;
    for journey in &state.active_journeys {
        validate_journey(
            state,
            journey,
            &train_ids,
            &service_ids,
            &station_ids,
            &service_distances,
        )?;
    }

    Ok(())
}

fn validate_rail_authority_finances(authority: &RailAuthority) -> Result<(), SaveValidationError> {
    if authority.construction_capacity == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "Rail Authority construction capacity",
        });
    }
    if authority.active_construction_count() > authority.construction_capacity {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority active construction exceeds its capacity",
        });
    }
    if authority.reserved_construction_count() > authority.construction_capacity {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority scheduled and active construction exceeds its capacity",
        });
    }

    let finances = &authority.finances;
    if finances.treasury.cents() < 0
        || finances.maintenance_reserve.cents() < 0
        || finances.committed_investment.cents() < 0
        || finances.carried_over_funds.cents() < 0
        || finances.regional_public_allocation.cents() < 0
        || finances.infrastructure_access_fee_revenue.cents() < 0
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Rail Authority financial amount",
        });
    }

    let earmarked = finances
        .maintenance_reserve
        .checked_add(finances.committed_investment)?;
    if earmarked > finances.treasury {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority earmarks exceed its treasury",
        });
    }
    if finances.carried_over_funds > finances.treasury {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Rail Authority carry-over exceeds its treasury",
        });
    }

    Ok(())
}

fn validate_infrastructure_projects(
    authority: &RailAuthority,
    settlement_ids: &HashSet<SettlementId>,
    station_ids: &HashSet<RailStationId>,
    line_ids: &HashSet<RailLineId>,
) -> Result<(), SaveValidationError> {
    let project_ids = unique_ids(
        authority
            .infrastructure_projects
            .iter()
            .map(|project| project.id),
        "Infrastructure Project",
    )?;
    if project_ids.iter().any(|id| !id.is_v4()) {
        return Err(SaveValidationError::InvalidId {
            kind: "Infrastructure Project",
        });
    }

    let mut total_project_commitments = Money::ZERO;
    for project in &authority.infrastructure_projects {
        if project.funding.estimated_cost.cents() < 0
            || project.funding.authority_committed.cents() < 0
            || project.funding.operator_contributed.cents() < 0
            || project.funding.access_fee_credit_awarded.cents() < 0
            || project.funding.access_fee_credit_remaining.cents() < 0
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Infrastructure Project funding amount",
            });
        }
        if project.funding.total_funded()? > project.funding.estimated_cost {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project funding exceeds estimated cost",
            });
        }
        if project.funding.operator_contributed > project.funding.operator_contribution_cap()? {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project operator contribution exceeds its cap",
            });
        }
        if project.funding.access_fee_credit_remaining > project.funding.access_fee_credit_awarded {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Infrastructure Project access credit remaining exceeds awarded credit",
            });
        }
        if let Some(discount) = project.funding.access_fee_discount {
            if discount.basis_points == 0 || discount.basis_points > 10_000 {
                return Err(SaveValidationError::InvalidValue {
                    field: "Infrastructure Project access discount basis points",
                });
            }
            if project.funding.operator_contributed <= Money::ZERO {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "Infrastructure Project access discount requires operator contribution",
                });
            }
        }
        if !matches!(
            project.status,
            InfrastructureProjectStatus::Open
                | InfrastructureProjectStatus::Rejected
                | InfrastructureProjectStatus::Cancelled
        ) {
            total_project_commitments =
                total_project_commitments.checked_add(project.funding.authority_committed)?;
        }
    }
    if total_project_commitments > authority.finances.committed_investment {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Infrastructure Project commitments exceed Authority committed investment",
        });
    }

    let mut reserved_station_ids = HashSet::new();
    let mut reserved_line_ids = HashSet::new();
    for project in &authority.infrastructure_projects {
        match &project.kind {
            InfrastructureProjectKind::NewLine {
                planned_stations,
                planned_lines,
            } => {
                if planned_lines.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "New Line planned Rail Lines",
                    });
                }
                let local_planned_station_ids = unique_ids(
                    planned_stations.iter().map(|station| station.id),
                    "planned Rail Station",
                )?;
                for station in planned_stations {
                    if !station.id.is_v4()
                        || (project.status != InfrastructureProjectStatus::Open
                            && station_ids.contains(&station.id))
                        || !reserved_station_ids.insert(station.id)
                    {
                        return Err(SaveValidationError::InvalidId {
                            kind: "planned Rail Station",
                        });
                    }
                    if !settlement_ids.contains(&station.settlement_id) {
                        return Err(SaveValidationError::DanglingReference {
                            field: "planned Rail Station Settlement",
                        });
                    }
                }
                unique_ids(
                    planned_lines.iter().map(|line| line.id),
                    "planned Rail Line",
                )?;
                for line in planned_lines {
                    if !line.id.is_v4()
                        || (project.status != InfrastructureProjectStatus::Open
                            && line_ids.contains(&line.id))
                        || !reserved_line_ids.insert(line.id)
                    {
                        return Err(SaveValidationError::InvalidId {
                            kind: "planned Rail Line",
                        });
                    }
                    let endpoint_exists = |station_id: RailStationId| {
                        station_ids.contains(&station_id)
                            || local_planned_station_ids.contains(&station_id)
                    };
                    if !endpoint_exists(line.first_station_id)
                        || !endpoint_exists(line.second_station_id)
                    {
                        return Err(SaveValidationError::DanglingReference {
                            field: "planned Rail Line endpoint",
                        });
                    }
                    if line.first_station_id == line.second_station_id {
                        return Err(SaveValidationError::ImpossibleState {
                            reason: "a planned Rail Line has the same Rail Station at both endpoints",
                        });
                    }
                }
            }
            InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
            | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
            | InfrastructureProjectKind::Electrification { rail_line_ids }
            | InfrastructureProjectKind::Renewal { rail_line_ids } => {
                if rail_line_ids.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "Infrastructure Project Rail Lines",
                    });
                }
                unique_ids(
                    rail_line_ids.iter().copied(),
                    "Infrastructure Project Rail Line",
                )?;
                if rail_line_ids.iter().any(|id| !line_ids.contains(id)) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "Infrastructure Project Rail Line",
                    });
                }
            }
            InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
                if rail_station_ids.is_empty() {
                    return Err(SaveValidationError::InvalidValue {
                        field: "Infrastructure Project Rail Stations",
                    });
                }
                unique_ids(
                    rail_station_ids.iter().copied(),
                    "Infrastructure Project Rail Station",
                )?;
                if rail_station_ids.iter().any(|id| !station_ids.contains(id)) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "Infrastructure Project Rail Station",
                    });
                }
            }
        }
    }

    for (index, project) in authority.infrastructure_projects.iter().enumerate() {
        if project.status == InfrastructureProjectStatus::Scheduled {
            if !project.funding.is_fully_funded()
                || project.timeline.funding_completed_at.is_none()
                || project.timeline.scheduled_start_at.is_none()
            {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "scheduled Infrastructure Project is not fully funded and scheduled",
                });
            }
        }

        if project.status == InfrastructureProjectStatus::Construction {
            let (Some(scheduled_start), Some(construction_started), Some(planned_completion)) = (
                project.timeline.scheduled_start_at,
                project.timeline.construction_started_at,
                project.timeline.planned_completion_at,
            ) else {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "Infrastructure Project under construction is missing construction timestamps",
                });
            };
            if !project.funding.is_fully_funded()
                || project.timeline.funding_completed_at.is_none()
                || construction_started < scheduled_start
                || planned_completion <= construction_started
            {
                return Err(SaveValidationError::ImpossibleState {
                    reason: "Infrastructure Project has an invalid construction lifecycle",
                });
            }
        }

        if !project.status.reserves_construction_capacity() {
            continue;
        }
        if authority.infrastructure_projects[index + 1..]
            .iter()
            .any(|other| {
                other.status.reserves_construction_capacity() && project.conflicts_with(other)
            })
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "conflicting Infrastructure Projects reserve construction at the same time",
            });
        }
    }

    Ok(())
}

fn unique_ids<T>(
    ids: impl IntoIterator<Item = T>,
    kind: &'static str,
) -> Result<HashSet<T>, SaveValidationError>
where
    T: Copy + Eq + Hash,
{
    let mut unique = HashSet::new();
    for id in ids {
        if !unique.insert(id) {
            return Err(SaveValidationError::DuplicateId { kind });
        }
    }
    Ok(unique)
}

fn validate_rules(state: &GameState) -> Result<(), SaveValidationError> {
    let balance = &state.rules.balance;
    if balance.starting_company_funds().cents() < 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "starting Company Funds",
        });
    }
    if state.rules.demand.cap_duration.seconds() == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "demand cap duration",
        });
    }

    let authority = &state.rules.authority;
    let authority_durations = [
        (
            "Authority request queue delay",
            authority.request_queue_delay(),
        ),
        ("Authority review duration", authority.review_duration()),
        ("Authority proposal duration", authority.proposal_duration()),
        (
            "Authority request cooldown",
            authority.council_request_cooldown(),
        ),
        (
            "Authority deferred reconsideration delay",
            authority.deferred_reconsideration_delay(),
        ),
        (
            "Authority mobilisation delay",
            authority.construction_mobilisation_delay(),
        ),
        (
            "Authority base construction duration",
            authority.new_line_base_construction_duration(),
        ),
    ];
    if let Some((field, _)) = authority_durations
        .into_iter()
        .find(|(_, duration)| duration.seconds() == 0)
    {
        return Err(SaveValidationError::InvalidValue { field });
    }
    if authority.low_difficulty_seconds_per_kilometre() == 0
        || authority.moderate_difficulty_seconds_per_kilometre() == 0
        || authority.high_difficulty_seconds_per_kilometre() == 0
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Authority construction rate",
        });
    }
    if authority.max_active_expansion_projects() == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "Authority active expansion project limit",
        });
    }
    Ok(())
}

fn service_distance(
    service: &PassengerService,
    network: &RailNetwork,
    line_ids: &HashSet<RailLineId>,
) -> Result<DistanceMetres, SaveValidationError> {
    if service.stop_station_ids.len() < 2 || service.rail_line_ids.is_empty() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Passenger Service must have at least two stops and a Rail Line path",
        });
    }

    if service
        .rail_line_ids
        .iter()
        .any(|rail_line_id| !line_ids.contains(rail_line_id))
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Passenger Service Rail Line",
        });
    }

    let expected_path =
        service_path_for_stops(network, &service.stop_station_ids).map_err(|_| {
            SaveValidationError::ImpossibleState {
                reason: "Passenger Service stops do not form a simple continuous Rail Line path",
            }
        })?;
    if expected_path != service.rail_line_ids {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Passenger Service Rail Lines do not match its ordered stops",
        });
    }

    let total_metres = service
        .rail_line_ids
        .iter()
        .try_fold(0_u64, |total, rail_line_id| {
            let line = network
                .rail_lines
                .iter()
                .find(|line| line.id == *rail_line_id)
                .expect("a validated Rail Line ID resolves in the Rail Network");
            total
                .checked_add(line.distance.metres())
                .ok_or(SaveValidationError::Calculation(
                    CalculationError::Overflow {
                        operation: "Passenger Service path distance",
                    },
                ))
        })?;
    let metres = i64::try_from(total_metres).map_err(|_| {
        SaveValidationError::Calculation(CalculationError::Overflow {
            operation: "Passenger Service path distance",
        })
    })?;
    DistanceMetres::new(metres).map_err(|_| SaveValidationError::InvalidValue {
        field: "Passenger Service path distance",
    })
}

fn validate_demand(
    state: &GameState,
    station_ids: &HashSet<RailStationId>,
) -> Result<(), SaveValidationError> {
    let mut directional_pairs = HashSet::new();
    for demand in &state.origin_destination_demand {
        if !station_ids.contains(&demand.origin_station_id)
            || !station_ids.contains(&demand.destination_station_id)
        {
            return Err(SaveValidationError::DanglingReference {
                field: "origin-destination demand",
            });
        }
        if demand.origin_station_id == demand.destination_station_id {
            return Err(SaveValidationError::ImpossibleState {
                reason: "origin-destination demand has identical endpoints",
            });
        }
        if !directional_pairs.insert((demand.origin_station_id, demand.destination_station_id)) {
            return Err(SaveValidationError::ImpossibleState {
                reason: "duplicate directional Passenger Demand pool",
            });
        }
        let cap = waiting_passenger_cap(state, demand);
        if demand.waiting_passengers > cap {
            return Err(SaveValidationError::InvalidValue {
                field: "Waiting Passengers above demand cap",
            });
        }
        if (demand.waiting_passengers == cap && demand.fractional_passenger_seconds != 0)
            || (demand.waiting_passengers < cap && demand.fractional_passenger_seconds >= 3_600)
        {
            return Err(SaveValidationError::InvalidValue {
                field: "fractional Passenger Demand remainder",
            });
        }
    }
    Ok(())
}

fn validate_financials(state: &GameState) -> Result<(), SaveValidationError> {
    if state.player_company.funds.cents() < 0
        || state.financials.operating_revenue.cents() < 0
        || state.financials.infrastructure_access_fees.cents() < 0
        || state.financials.fuel_costs.cents() < 0
    {
        return Err(SaveValidationError::InvalidValue {
            field: "Company Funds or financial total",
        });
    }
    let station_ids = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect::<HashSet<_>>();
    let mut receipt_ids = HashSet::new();
    for receipt in &state.financials.recent_journey_receipts {
        if !receipt.journey_id.is_v4() || !receipt_ids.insert(receipt.journey_id) {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt ID",
            });
        }
        if receipt.revenue.cents() < 0
            || receipt.infrastructure_access_fee.cents() < 0
            || receipt.fuel_cost.cents() < 0
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt amount",
            });
        }
        receipt
            .infrastructure_access_fee
            .checked_add(receipt.fuel_cost)?;

        let metadata_fields_present = [
            receipt.train_id.is_some(),
            receipt.train_model_name.is_some(),
            receipt.origin_station_id.is_some(),
            receipt.destination_station_id.is_some(),
            receipt.passengers_carried.is_some(),
            receipt.passenger_capacity.is_some(),
            receipt.completed_at.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if metadata_fields_present != 0 && metadata_fields_present != 7 {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey receipt operating context",
            });
        }
        if metadata_fields_present == 7 {
            let train_id = receipt
                .train_id
                .expect("complete receipt context has Train ID");
            let train_model_name = receipt
                .train_model_name
                .as_deref()
                .expect("complete receipt context has Train model");
            let origin = receipt
                .origin_station_id
                .expect("complete receipt context has origin");
            let destination = receipt
                .destination_station_id
                .expect("complete receipt context has destination");
            let _passengers = receipt
                .passengers_carried
                .expect("complete receipt context has passengers");
            let capacity = receipt
                .passenger_capacity
                .expect("complete receipt context has capacity");
            if !train_id.is_v4()
                || train_model_name.trim().is_empty()
                || origin == destination
                || !station_ids.contains(&origin)
                || !station_ids.contains(&destination)
                || capacity == 0
            {
                return Err(SaveValidationError::InvalidValue {
                    field: "Journey receipt operating context",
                });
            }
        }
    }
    Ok(())
}

fn validate_service_assignments(
    state: &GameState,
    train_ids: &HashSet<TrainId>,
    service_ids: &HashSet<ServiceId>,
) -> Result<(), SaveValidationError> {
    for (train_id, service_id) in &state.player_company.fleet.service_assignments {
        if !train_ids.contains(train_id) {
            return Err(SaveValidationError::DanglingReference {
                field: "Train Passenger Service assignment Train",
            });
        }
        if !service_ids.contains(service_id) {
            return Err(SaveValidationError::DanglingReference {
                field: "Train Passenger Service assignment Service",
            });
        }

        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == *train_id)
            .expect("validated Train assignment resolves in Fleet");
        if let TrainStatus::Travelling { journey_id } = train.status
            && let Some(journey) = state
                .active_journeys
                .iter()
                .find(|journey| journey.id == journey_id)
            && journey.service_id != *service_id
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "a travelling Train is assigned to a different Passenger Service than its Journey",
            });
        }
    }
    Ok(())
}

fn validate_train_statuses(
    trains: &[Train],
    registration_code: u8,
    station_ids: &HashSet<RailStationId>,
    journey_ids: &HashSet<crate::model::JourneyId>,
) -> Result<(), SaveValidationError> {
    let mut travelling_journey_ids = HashSet::new();
    let mut vehicle_numbers = HashSet::new();
    for train in trains {
        let Some(model) = model_for_train(train) else {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue model reference",
            });
        };
        if train.model_id.as_str().trim().is_empty() || train.original_purchase_price.cents() <= 0 {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue model reference",
            });
        }
        let evn = EuropeanVehicleNumber::parse(train.evn.as_str()).map_err(|_| {
            SaveValidationError::InvalidValue {
                field: "European Vehicle Number",
            }
        })?;
        if !vehicle_numbers.insert(evn.as_str().to_owned()) {
            return Err(SaveValidationError::DuplicateId {
                kind: "European Vehicle Number",
            });
        }
        if evn.vehicle_type_code() != model.evn_type_code()
            || evn.registration_code() != registration_code
            || evn.series_code() != model.evn_series_code()
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Train European Vehicle Number does not match its model and Region",
            });
        }
        match train.status {
            TrainStatus::Ready { at } if !station_ids.contains(&at) => {
                return Err(SaveValidationError::DanglingReference {
                    field: "READY Train location",
                });
            }
            TrainStatus::Travelling { journey_id } => {
                if !journey_ids.contains(&journey_id) {
                    return Err(SaveValidationError::DanglingReference {
                        field: "travelling Train Journey",
                    });
                }
                if !travelling_journey_ids.insert(journey_id) {
                    return Err(SaveValidationError::ImpossibleState {
                        reason: "more than one Train is travelling on one Journey",
                    });
                }
            }
            TrainStatus::Ready { .. } => {}
        }
    }
    Ok(())
}

fn validate_journey(
    state: &GameState,
    journey: &Journey,
    train_ids: &HashSet<TrainId>,
    service_ids: &HashSet<ServiceId>,
    station_ids: &HashSet<RailStationId>,
    service_distances: &HashMap<ServiceId, DistanceMetres>,
) -> Result<(), SaveValidationError> {
    if !train_ids.contains(&journey.train_id) {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey Train",
        });
    }
    if !service_ids.contains(&journey.service_id) {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey Passenger Service",
        });
    }
    if !station_ids.contains(&journey.origin_station_id)
        || !station_ids.contains(&journey.destination_station_id)
    {
        return Err(SaveValidationError::DanglingReference {
            field: "Journey endpoint",
        });
    }

    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == journey.train_id)
        .expect("a validated Journey Train ID resolves in the Fleet");
    if train.status
        != (TrainStatus::Travelling {
            journey_id: journey.id,
        })
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Journey's Train is not travelling on that Journey",
        });
    }
    let train_model = model_for_train(train).ok_or(SaveValidationError::InvalidValue {
        field: "Journey Train catalogue model",
    })?;

    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .expect("a validated Journey Passenger Service ID resolves in the Service Network");

    if journey.purpose == JourneyPurpose::Positioning {
        if state
            .player_company
            .fleet
            .assigned_service_id(journey.train_id)
            != Some(journey.service_id)
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "positioning Journey Train is not assigned to its Passenger Service",
            });
        }
        if !service.accepts_departure_station(journey.destination_station_id)
            || journey.origin_station_id == journey.destination_station_id
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "positioning Journey does not end at a valid Service departure terminus",
            });
        }
        path_between_stations(
            &state.region.rail_authority.rail_network,
            journey.origin_station_id,
            journey.destination_station_id,
        )
        .map_err(|_| SaveValidationError::ImpossibleState {
            reason: "positioning Journey has no open Rail Line path",
        })?;
        if journey.passengers_carried != 0
            || !journey.passenger_groups.is_empty()
            || journey.fare != Money::ZERO
            || journey.operating_revenue != Money::ZERO
            || journey.credited_revenue != Money::ZERO
            || journey.infrastructure_access_fee < Money::ZERO
            || journey.fuel_cost.cents() <= 0
            || journey.current_stop_index != 0
            || journey.arrives_at <= journey.departed_at
        {
            return Err(SaveValidationError::ImpossibleState {
                reason: "positioning Journey actuals do not match an empty non-revenue movement",
            });
        }
        return Ok(());
    }

    let Some(service_origin) = service.origin_station_id() else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey references a Passenger Service without an origin",
        });
    };
    let Some(service_destination) = service.destination_station_id() else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey references a Passenger Service without a destination",
        });
    };
    let direction = if journey.origin_station_id == service_origin
        && journey.destination_station_id == service_destination
    {
        1_i32
    } else if journey.origin_station_id == service_destination
        && journey.destination_station_id == service_origin
    {
        -1_i32
    } else {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey endpoints do not match its Passenger Service",
        });
    };

    let next_stop_index = match direction {
        1 => journey
            .current_stop_index
            .checked_add(1)
            .filter(|index| *index < service.stop_station_ids.len()),
        -1 => journey.current_stop_index.checked_sub(1),
        _ => None,
    }
    .ok_or(SaveValidationError::ImpossibleState {
        reason: "Journey current Service stop cannot advance",
    })?;

    let distance = service_distances
        .get(&journey.service_id)
        .copied()
        .expect("every validated Passenger Service has a calculated distance");
    let expected_through_fare = journey.fare_rate.checked_charge(distance)?;

    if journey.fare != expected_through_fare
        || journey.infrastructure_access_fee < Money::ZERO
        || journey.fuel_cost.cents() <= 0
        || journey.operating_revenue.cents() < 0
        || journey.credited_revenue.cents() < 0
        || journey.credited_revenue > journey.operating_revenue
        || journey.arrives_at <= journey.departed_at
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey actuals do not match its saved rules and departure snapshot",
        });
    }

    let mut onboard_passengers = 0_u32;
    let mut onboard_revenue = Money::ZERO;
    for group in &journey.passenger_groups {
        if group.passengers == 0
            || !station_ids.contains(&group.origin_station_id)
            || !station_ids.contains(&group.destination_station_id)
        {
            return Err(SaveValidationError::InvalidValue {
                field: "Journey passenger group",
            });
        }

        let Some(origin_index) = service
            .stop_station_ids
            .iter()
            .position(|station_id| *station_id == group.origin_station_id)
        else {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger origin is not a Service stop",
            });
        };
        let Some(destination_index) = service
            .stop_station_ids
            .iter()
            .position(|station_id| *station_id == group.destination_station_id)
        else {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger destination is not a Service stop",
            });
        };

        let valid_group_direction = if direction > 0 {
            origin_index < destination_index
                && destination_index >= next_stop_index
                && origin_index <= journey.current_stop_index
        } else {
            origin_index > destination_index
                && destination_index <= next_stop_index
                && origin_index >= journey.current_stop_index
        };
        if !valid_group_direction {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger group is not travelling in the Service direction",
            });
        }

        let group_distance = distance_between_service_stops(
            &state.region.rail_authority.rail_network,
            service,
            origin_index,
            destination_index,
        )?;
        let expected_group_fare = journey.fare_rate.checked_charge(group_distance)?;
        if group.fare != expected_group_fare {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger fare does not match its origin-destination distance",
            });
        }

        onboard_passengers = onboard_passengers.checked_add(group.passengers).ok_or(
            SaveValidationError::Calculation(CalculationError::Overflow {
                operation: "Journey onboard passenger count",
            }),
        )?;
        onboard_revenue =
            onboard_revenue.checked_add(group.fare.checked_mul(u64::from(group.passengers))?)?;
    }

    if onboard_passengers > train_model.passenger_capacity().passengers()
        || journey.passengers_carried < onboard_passengers
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey passenger counts exceed Train capacity or cumulative boardings",
        });
    }
    let expected_booked_revenue = journey.credited_revenue.checked_add(onboard_revenue)?;
    if expected_booked_revenue != journey.operating_revenue {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey booked revenue does not match credited and onboard passengers",
        });
    }

    // Demand pools must exist for every currently onboard OD pair. Their
    // waiting counts need not match the departure snapshot because demand keeps
    // replenishing while the Journey is active.
    for group in &journey.passenger_groups {
        if !state.origin_destination_demand.iter().any(|demand| {
            demand.origin_station_id == group.origin_station_id
                && demand.destination_station_id == group.destination_station_id
        }) {
            return Err(SaveValidationError::ImpossibleState {
                reason: "Journey passenger group has no saved Passenger Demand",
            });
        }
    }

    Ok(())
}

fn distance_between_service_stops(
    network: &RailNetwork,
    service: &PassengerService,
    first_stop_index: usize,
    second_stop_index: usize,
) -> Result<DistanceMetres, SaveValidationError> {
    let first_station_id = *service.stop_station_ids.get(first_stop_index).ok_or(
        SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        },
    )?;
    let second_station_id = *service.stop_station_ids.get(second_stop_index).ok_or(
        SaveValidationError::ImpossibleState {
            reason: "Passenger Service stop index is outside the stop pattern",
        },
    )?;
    let line_ids =
        path_between_stations(network, first_station_id, second_station_id).map_err(|_| {
            SaveValidationError::ImpossibleState {
                reason: "Passenger Service stops are not connected by the Rail Network",
            }
        })?;
    let total_metres = line_ids.iter().try_fold(0_u64, |total, rail_line_id| {
        let line = network
            .rail_lines
            .iter()
            .find(|line| line.id == *rail_line_id)
            .ok_or(SaveValidationError::DanglingReference {
                field: "Passenger Service Rail Line",
            })?;
        total
            .checked_add(line.distance.metres())
            .ok_or(SaveValidationError::Calculation(
                CalculationError::Overflow {
                    operation: "Passenger Service stop distance",
                },
            ))
    })?;
    let metres = i64::try_from(total_metres).map_err(|_| {
        SaveValidationError::Calculation(CalculationError::Overflow {
            operation: "Passenger Service stop distance",
        })
    })?;
    DistanceMetres::new(metres).map_err(|_| SaveValidationError::InvalidValue {
        field: "Passenger Service stop distance",
    })
}

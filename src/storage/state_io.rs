use super::*;

pub(super) fn clear_state(
    transaction: &Transaction<'_>,
    path: &Path,
    preserve_receipt_history: bool,
) -> Result<(), SaveSlotError> {
    if !preserve_receipt_history {
        transaction
            .execute("DELETE FROM journey_receipts", [])
            .map_err(|source| SaveSlotError::Database {
                action: "clear Journey history from",
                path: path.to_path_buf(),
                source,
            })?;
    }
    transaction
        .execute_batch(
            "DELETE FROM journey_passenger_groups;
         DELETE FROM active_journeys;
         DELETE FROM origin_destination_demand;
         DELETE FROM service_lines;
         DELETE FROM service_stops;
         DELETE FROM passenger_services;
         DELETE FROM trains;
         DELETE FROM train_model_sequences;
         DELETE FROM financials;
         DELETE FROM game_rules;
         DELETE FROM company;
         DELETE FROM infrastructure_project_planned_lines;
         DELETE FROM infrastructure_project_planned_stations;
         DELETE FROM infrastructure_project_rail_stations;
         DELETE FROM infrastructure_project_rail_lines;
         DELETE FROM infrastructure_projects;
         DELETE FROM rail_authority_finances;
         DELETE FROM rail_lines;
         DELETE FROM rail_stations;
         DELETE FROM settlements;
         DELETE FROM bulletin_entries;
         DELETE FROM region;
         DELETE FROM game_meta;",
        )
        .map_err(|source| SaveSlotError::Database {
            action: "clear previous state from",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

pub(super) fn insert_state(
    transaction: &Transaction<'_>,
    state: &GameState,
    path: &Path,
) -> Result<(), SaveSlotError> {
    let db = |value: u64, field| to_db_u64(value, field, path);

    transaction
        .execute(
            "INSERT INTO game_meta(singleton, world_seed, last_processed_at) VALUES(1, ?1, ?2)",
            params![
                state.world_seed.to_string(),
                state.last_processed_at.unix_seconds()
            ],
        )
        .map_err(|source| db_error("write game metadata to", path, source))?;
    transaction
        .execute(
            "INSERT INTO region(
             singleton, name, registration_code, registration_mark, population,
             rail_authority_name, rail_authority_construction_capacity
         ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &state.region.name,
                i64::from(state.region.railway_registration.numeric_code),
                &state.region.railway_registration.mark,
                db(state.region.population, "Region Population")?,
                &state.region.rail_authority.name,
                i64::from(state.region.rail_authority.construction_capacity),
            ],
        )
        .map_err(|source| db_error("write Region to", path, source))?;

    for (sequence, entry) in state.region.bulletin.iter().enumerate() {
        let category = match entry.category {
            BulletinCategory::Local => "local",
            BulletinCategory::Authority => "authority",
            BulletinCategory::Construction => "construction",
            BulletinCategory::Network => "network",
        };
        transaction
            .execute(
                "INSERT INTO bulletin_entries(sequence, occurred_at, category, headline, detail)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    entry.occurred_at.unix_seconds(),
                    category,
                    &entry.headline,
                    &entry.detail,
                ],
            )
            .map_err(|source| db_error("write Bulletin entries to", path, source))?;
    }

    let authority_finances = &state.region.rail_authority.finances;
    transaction
        .execute(
            "INSERT INTO rail_authority_finances(
                 singleton, treasury_cents, maintenance_reserve_cents,
                 committed_investment_cents, carried_over_funds_cents,
                 regional_public_allocation_cents, infrastructure_access_fee_revenue_cents,
                 next_fiscal_period_at
             ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                authority_finances.treasury.cents(),
                authority_finances.maintenance_reserve.cents(),
                authority_finances.committed_investment.cents(),
                authority_finances.carried_over_funds.cents(),
                authority_finances.regional_public_allocation.cents(),
                authority_finances.infrastructure_access_fee_revenue.cents(),
                authority_finances
                    .next_fiscal_period_at
                    .map(UtcSeconds::unix_seconds),
            ],
        )
        .map_err(|source| db_error("write Rail Authority finances to", path, source))?;

    for (sequence, settlement) in state.region.settlements.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO settlements(id, sequence, name, population, world_x, world_y) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    settlement.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &settlement.name,
                    db(settlement.population, "Settlement Population")?,
                    settlement.position.x,
                    settlement.position.y
                ],
            )
            .map_err(|source| db_error("write Settlements to", path, source))?;
    }
    let network = &state.region.rail_authority.rail_network;
    for (sequence, station) in network.rail_stations.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO rail_stations(id, sequence, settlement_id) VALUES(?1, ?2, ?3)",
                params![
                    station.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    station.settlement_id.to_string()
                ],
            )
            .map_err(|source| db_error("write Rail Stations to", path, source))?;
    }
    for (sequence, line) in network.rail_lines.iter().enumerate() {
        let electrification = match line.electrification {
            Electrification::None => "none",
            Electrification::Electric => "electric",
        };
        let construction_difficulty = match line.construction_difficulty {
            ConstructionDifficulty::Low => "low",
            ConstructionDifficulty::Moderate => "moderate",
            ConstructionDifficulty::High => "high",
        };
        transaction
            .execute(
                "INSERT INTO rail_lines(
                 id, sequence, first_station_id, second_station_id, distance_metres,
                 speed_limit_kmh, track_count, electrification, construction_difficulty
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    line.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    line.first_station_id.to_string(),
                    line.second_station_id.to_string(),
                    db(line.distance.metres(), "Rail Line distance")?,
                    i64::from(line.speed_limit.kilometres_per_hour()),
                    i64::from(line.track_count.tracks()),
                    electrification,
                    construction_difficulty,
                ],
            )
            .map_err(|source| db_error("write Rail Lines to", path, source))?;
    }

    for (sequence, project) in state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .enumerate()
    {
        insert_infrastructure_project(transaction, sequence, project, path)?;
    }

    transaction
        .execute(
            "INSERT INTO company(singleton, name, vkm, funds_cents, next_train_display_number)
         VALUES(1, ?1, ?2, ?3, ?4)",
            params![
                &state.player_company.name,
                state.player_company.vehicle_keeper_mark.as_str(),
                state.player_company.funds.cents(),
                to_db_u64(
                    state.player_company.fleet.next_train_display_number,
                    "next Train display number",
                    path,
                )?,
            ],
        )
        .map_err(|source| db_error("write Player Company to", path, source))?;

    for (model_id, next_unit_number) in &state.player_company.fleet.next_evn_unit_by_model {
        transaction
            .execute(
                "INSERT INTO train_model_sequences(model_id, next_unit_number) VALUES(?1, ?2)",
                params![model_id.as_str(), i64::from(*next_unit_number)],
            )
            .map_err(|source| db_error("write EVN model sequences to", path, source))?;
    }

    for (sequence, train) in state.player_company.fleet.trains.iter().enumerate() {
        let (status_kind, status_ref_id) = match train.status {
            TrainStatus::Ready { at } => ("ready", at.to_string()),
            TrainStatus::Travelling { journey_id } => ("travelling", journey_id.to_string()),
        };
        transaction.execute(
            "INSERT INTO trains(id, sequence, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                train.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), train.evn.as_str(),
                train.nickname.as_ref().map(TrainNickname::as_str), status_kind,
                status_ref_id, train.model_id.as_str(),
                train.original_purchase_price.cents()
            ],
        ).map_err(|source| db_error("write Trains to", path, source))?;
    }

    for (sequence, service) in state.player_company.passenger_services.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO passenger_services(id, sequence, name) VALUES(?1, ?2, ?3)",
                params![
                    service.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    &service.name
                ],
            )
            .map_err(|source| db_error("write Passenger Services to", path, source))?;
        for (sequence, station_id) in service.stop_station_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_stops(service_id, sequence, station_id) VALUES(?1, ?2, ?3)",
                params![service.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), station_id.to_string()],
            ).map_err(|source| db_error("write Passenger Service stops to", path, source))?;
        }
        for (sequence, line_id) in service.rail_line_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO service_lines(service_id, sequence, rail_line_id) VALUES(?1, ?2, ?3)",
                params![service.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), line_id.to_string()],
            ).map_err(|source| db_error("write Passenger Service paths to", path, source))?;
        }
    }

    for (sequence, demand) in state.origin_destination_demand.iter().enumerate() {
        transaction.execute(
            "INSERT INTO origin_destination_demand(origin_station_id, destination_station_id, sequence, waiting_passengers, market_maturity_basis_points, passenger_arrival_rate_per_hour, fractional_passenger_seconds)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                demand.origin_station_id.to_string(), demand.destination_station_id.to_string(),
                i64::try_from(sequence).unwrap_or(i64::MAX),
                i64::from(demand.waiting_passengers), i64::from(demand.market_maturity.basis_points()),
                i64::from(demand.passenger_arrival_rate_per_hour.passengers_per_hour()),
                db(demand.fractional_passenger_seconds, "Demand fractional passenger seconds")?
            ],
        ).map_err(|source| db_error("write Passenger Demand to", path, source))?;
    }

    for (sequence, journey) in state.active_journeys.iter().enumerate() {
        transaction.execute(
            "INSERT INTO active_journeys(id, sequence, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                journey.id.to_string(), i64::try_from(sequence).unwrap_or(i64::MAX), journey.service_id.to_string(), journey.train_id.to_string(),
                journey.origin_station_id.to_string(), journey.destination_station_id.to_string(), i64::from(journey.passengers_carried),
                journey.fare.cents(), journey.operating_revenue.cents(), journey.credited_revenue.cents(),
                journey.infrastructure_access_fee.cents(), journey.fuel_cost.cents(),
                i64::try_from(journey.current_stop_index).map_err(|_| SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::InvalidValue { field: "Journey current stop index" }),
                })?,
                journey.departed_at.unix_seconds(), journey.arrives_at.unix_seconds()
            ],
        ).map_err(|source| db_error("write active Journeys to", path, source))?;

        for (sequence, group) in journey.passenger_groups.iter().enumerate() {
            transaction.execute(
                "INSERT INTO journey_passenger_groups(journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    journey.id.to_string(),
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    group.origin_station_id.to_string(),
                    group.destination_station_id.to_string(),
                    i64::from(group.passengers),
                    group.fare.cents(),
                ],
            ).map_err(|source| db_error("write Journey passenger groups to", path, source))?;
        }
    }

    transaction.execute(
        "INSERT INTO financials(singleton, operating_revenue_cents, infrastructure_access_fees_cents, fuel_costs_cents) VALUES(1, ?1, ?2, ?3)",
        params![state.financials.operating_revenue.cents(), state.financials.infrastructure_access_fees.cents(), state.financials.fuel_costs.cents()],
    ).map_err(|source| db_error("write financial totals to", path, source))?;
    for receipt in &state.financials.recent_journey_receipts {
        transaction.execute(
            "INSERT OR REPLACE INTO journey_receipts(journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                receipt.journey_id.to_string(), receipt.revenue.cents(), receipt.infrastructure_access_fee.cents(), receipt.fuel_cost.cents(),
                receipt.train_id.map(|id| id.to_string()), receipt.train_model_name.as_deref(),
                receipt.origin_station_id.map(|id| id.to_string()),
                receipt.destination_station_id.map(|id| id.to_string()),
                receipt.passengers_carried.map(i64::from), receipt.passenger_capacity.map(i64::from), receipt.completed_at.map(UtcSeconds::unix_seconds)
            ],
        ).map_err(|source| db_error("write Journey receipts to", path, source))?;
    }

    let balance = &state.rules.balance;
    transaction.execute(
        "INSERT INTO game_rules(singleton, fare_cents_per_passenger_km, access_fee_cents_per_train_km, starting_company_funds_cents, demand_cap_seconds)
         VALUES(1, ?1, ?2, ?3, ?4)",
        params![
            db(balance.fare_per_passenger_kilometre().cents_per_kilometre(), "fare rate")?,
            db(balance.access_fee_per_train_kilometre().cents_per_kilometre(), "access fee rate")?,
            balance.starting_company_funds().cents(), db(state.rules.demand.cap_duration.seconds(), "demand cap duration")?
        ],
    ).map_err(|source| db_error("write game rules to", path, source))?;
    Ok(())
}

fn insert_infrastructure_project(
    transaction: &Transaction<'_>,
    sequence: usize,
    project: &InfrastructureProject,
    path: &Path,
) -> Result<(), SaveSlotError> {
    let (kind, target_speed_limit_kmh, target_track_count) = match &project.kind {
        InfrastructureProjectKind::NewLine { .. } => ("new_line", None, None),
        InfrastructureProjectKind::SpeedUpgrade {
            target_speed_limit, ..
        } => (
            "speed_upgrade",
            Some(i64::from(target_speed_limit.kilometres_per_hour())),
            None,
        ),
        InfrastructureProjectKind::DoubleTracking {
            target_track_count, ..
        } => (
            "double_tracking",
            None,
            Some(i64::from(target_track_count.tracks())),
        ),
        InfrastructureProjectKind::Electrification { .. } => ("electrification", None, None),
        InfrastructureProjectKind::Renewal { .. } => ("renewal", None, None),
        InfrastructureProjectKind::StationUpgrade { .. } => ("station_upgrade", None, None),
    };
    let status = match project.status {
        InfrastructureProjectStatus::Requested => "requested",
        InfrastructureProjectStatus::UnderReview => "under_review",
        InfrastructureProjectStatus::Proposed => "proposed",
        InfrastructureProjectStatus::Approved => "approved",
        InfrastructureProjectStatus::Deferred => "deferred",
        InfrastructureProjectStatus::Funding => "funding",
        InfrastructureProjectStatus::Scheduled => "scheduled",
        InfrastructureProjectStatus::Construction => "construction",
        InfrastructureProjectStatus::Open => "open",
        InfrastructureProjectStatus::Cancelled => "cancelled",
    };
    let timeline = &project.timeline;
    transaction
        .execute(
            "INSERT INTO infrastructure_projects(
                 id, sequence, kind, status, estimated_cost_cents, authority_committed_cents,
                 operator_contributed_cents, access_fee_credit_awarded_cents, access_fee_credit_remaining_cents,
                 requested_at, review_started_at, proposed_at, approved_at, funding_completed_at, scheduled_start_at,
                 construction_started_at, planned_completion_at, completed_at, deferred_at, cancelled_at,
                 reconsideration_count, target_speed_limit_kmh, target_track_count
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                project.id.to_string(),
                i64::try_from(sequence).unwrap_or(i64::MAX),
                kind,
                status,
                project.funding.estimated_cost.cents(),
                project.funding.authority_committed.cents(),
                project.funding.operator_contributed.cents(),
                project.funding.access_fee_credit_awarded.cents(),
                project.funding.access_fee_credit_remaining.cents(),
                timeline.requested_at.unix_seconds(),
                timeline.review_started_at.map(UtcSeconds::unix_seconds),
                timeline.proposed_at.map(UtcSeconds::unix_seconds),
                timeline.approved_at.map(UtcSeconds::unix_seconds),
                timeline.funding_completed_at.map(UtcSeconds::unix_seconds),
                timeline.scheduled_start_at.map(UtcSeconds::unix_seconds),
                timeline.construction_started_at.map(UtcSeconds::unix_seconds),
                timeline.planned_completion_at.map(UtcSeconds::unix_seconds),
                timeline.completed_at.map(UtcSeconds::unix_seconds),
                timeline.deferred_at.map(UtcSeconds::unix_seconds),
                timeline.cancelled_at.map(UtcSeconds::unix_seconds),
                i64::from(timeline.reconsideration_count),
                target_speed_limit_kmh,
                target_track_count,
            ],
        )
        .map_err(|source| db_error("write infrastructure projects to", path, source))?;

    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            for (sequence, station) in planned_stations.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_planned_stations(
                             project_id, sequence, station_id, settlement_id
                         ) VALUES(?1, ?2, ?3, ?4)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            station.id.to_string(),
                            station.settlement_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write planned Rail Stations to", path, source))?;
            }
            for (sequence, line) in planned_lines.iter().enumerate() {
                let electrification = match line.electrification {
                    Electrification::None => "none",
                    Electrification::Electric => "electric",
                };
                let construction_difficulty = match line.construction_difficulty {
                    ConstructionDifficulty::Low => "low",
                    ConstructionDifficulty::Moderate => "moderate",
                    ConstructionDifficulty::High => "high",
                };
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_planned_lines(
                             project_id, sequence, line_id, first_station_id, second_station_id,
                             distance_metres, speed_limit_kmh, track_count, electrification,
                             construction_difficulty
                         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            line.id.to_string(),
                            line.first_station_id.to_string(),
                            line.second_station_id.to_string(),
                            to_db_u64(line.distance.metres(), "planned Rail Line distance", path)?,
                            i64::from(line.speed_limit.kilometres_per_hour()),
                            i64::from(line.track_count.tracks()),
                            electrification,
                            construction_difficulty,
                        ],
                    )
                    .map_err(|source| db_error("write planned Rail Lines to", path, source))?;
            }
        }
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. }
        | InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. }
        | InfrastructureProjectKind::Electrification { rail_line_ids }
        | InfrastructureProjectKind::Renewal { rail_line_ids } => {
            for (sequence, rail_line_id) in rail_line_ids.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_rail_lines(project_id, sequence, rail_line_id)
                         VALUES(?1, ?2, ?3)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            rail_line_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write infrastructure project Rail Lines to", path, source))?;
            }
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            for (sequence, rail_station_id) in rail_station_ids.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO infrastructure_project_rail_stations(project_id, sequence, rail_station_id)
                         VALUES(?1, ?2, ?3)",
                        params![
                            project.id.to_string(),
                            i64::try_from(sequence).unwrap_or(i64::MAX),
                            rail_station_id.to_string(),
                        ],
                    )
                    .map_err(|source| db_error("write infrastructure project Rail Stations to", path, source))?;
            }
        }
    }

    Ok(())
}

fn load_infrastructure_projects(
    connection: &Connection,
    path: &Path,
) -> Result<Vec<InfrastructureProject>, SaveSlotError> {
    #[derive(Debug)]
    struct PersistedProject {
        id: InfrastructureProjectId,
        kind: String,
        status: String,
        funding: InfrastructureProjectFunding,
        timeline: InfrastructureProjectTimeline,
        target_speed_limit_kmh: Option<i64>,
        target_track_count: Option<i64>,
    }

    let rows = query_all(
        connection,
        "SELECT id, kind, status, estimated_cost_cents, authority_committed_cents,
                operator_contributed_cents, access_fee_credit_awarded_cents, access_fee_credit_remaining_cents,
                requested_at, review_started_at, proposed_at, approved_at, funding_completed_at, scheduled_start_at,
                construction_started_at, planned_completion_at, completed_at, deferred_at, cancelled_at,
                reconsideration_count, target_speed_limit_kmh, target_track_count
         FROM infrastructure_projects ORDER BY sequence",
        path,
        |row| {
            let timestamp = |index: usize| -> rusqlite::Result<Option<UtcSeconds>> {
                Ok(row
                    .get::<_, Option<i64>>(index)?
                    .map(UtcSeconds::from_unix_seconds))
            };
            Ok(PersistedProject {
                id: row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                kind: row.get(1)?,
                status: row.get(2)?,
                funding: InfrastructureProjectFunding {
                    estimated_cost: Money::from_cents(row.get(3)?),
                    authority_committed: Money::from_cents(row.get(4)?),
                    operator_contributed: Money::from_cents(row.get(5)?),
                    access_fee_credit_awarded: Money::from_cents(row.get(6)?),
                    access_fee_credit_remaining: Money::from_cents(row.get(7)?),
                },
                timeline: InfrastructureProjectTimeline {
                    requested_at: UtcSeconds::from_unix_seconds(row.get(8)?),
                    review_started_at: timestamp(9)?,
                    proposed_at: timestamp(10)?,
                    approved_at: timestamp(11)?,
                    funding_completed_at: timestamp(12)?,
                    scheduled_start_at: timestamp(13)?,
                    construction_started_at: timestamp(14)?,
                    planned_completion_at: timestamp(15)?,
                    completed_at: timestamp(16)?,
                    deferred_at: timestamp(17)?,
                    cancelled_at: timestamp(18)?,
                    reconsideration_count: u8::try_from(row.get::<_, i64>(19)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                target_speed_limit_kmh: row.get(20)?,
                target_track_count: row.get(21)?,
            })
        },
    )?;

    let mut line_targets: HashMap<InfrastructureProjectId, Vec<RailLineId>> = HashMap::new();
    for (project_id, rail_line_id) in query_all(
        connection,
        "SELECT project_id, rail_line_id
         FROM infrastructure_project_rail_lines ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                row_domain_id(row, 1, "Rail Line ID", RailLineId::parse)?,
            ))
        },
    )? {
        line_targets
            .entry(project_id)
            .or_default()
            .push(rail_line_id);
    }

    let mut station_targets: HashMap<InfrastructureProjectId, Vec<RailStationId>> = HashMap::new();
    for (project_id, rail_station_id) in query_all(
        connection,
        "SELECT project_id, rail_station_id
         FROM infrastructure_project_rail_stations ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                row_domain_id(row, 1, "Rail Station ID", RailStationId::parse)?,
            ))
        },
    )? {
        station_targets
            .entry(project_id)
            .or_default()
            .push(rail_station_id);
    }

    let mut planned_stations: HashMap<InfrastructureProjectId, Vec<PlannedRailStation>> =
        HashMap::new();
    for (project_id, station) in query_all(
        connection,
        "SELECT project_id, station_id, settlement_id
         FROM infrastructure_project_planned_stations ORDER BY project_id, sequence",
        path,
        |row| {
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                PlannedRailStation {
                    id: row_domain_id(row, 1, "planned Rail Station ID", RailStationId::parse)?,
                    settlement_id: row_domain_id(row, 2, "Settlement ID", SettlementId::parse)?,
                },
            ))
        },
    )? {
        planned_stations
            .entry(project_id)
            .or_default()
            .push(station);
    }

    let mut planned_lines: HashMap<InfrastructureProjectId, Vec<PlannedRailLine>> = HashMap::new();
    for (project_id, line) in query_all(
        connection,
        "SELECT project_id, line_id, first_station_id, second_station_id, distance_metres,
                speed_limit_kmh, track_count, electrification, construction_difficulty
         FROM infrastructure_project_planned_lines ORDER BY project_id, sequence",
        path,
        |row| {
            let electrification = match row.get::<_, String>(7)?.as_str() {
                "none" => Electrification::None,
                "electric" => Electrification::Electric,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let construction_difficulty = match row.get::<_, String>(8)?.as_str() {
                "low" => ConstructionDifficulty::Low,
                "moderate" => ConstructionDifficulty::Moderate,
                "high" => ConstructionDifficulty::High,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok((
                row_domain_id(
                    row,
                    0,
                    "Infrastructure Project ID",
                    InfrastructureProjectId::parse,
                )?,
                PlannedRailLine {
                    id: row_domain_id(row, 1, "planned Rail Line ID", RailLineId::parse)?,
                    first_station_id: row_domain_id(
                        row,
                        2,
                        "planned Rail Line endpoint",
                        RailStationId::parse,
                    )?,
                    second_station_id: row_domain_id(
                        row,
                        3,
                        "planned Rail Line endpoint",
                        RailStationId::parse,
                    )?,
                    distance: DistanceMetres::new(row.get(4)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    speed_limit: SpeedKilometresPerHour::new(row.get(5)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    track_count: TrackCount::new(row.get(6)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    electrification,
                    construction_difficulty,
                },
            ))
        },
    )? {
        planned_lines.entry(project_id).or_default().push(line);
    }

    let mut projects = Vec::with_capacity(rows.len());
    for row in rows {
        let status = match row.status.as_str() {
            "requested" => InfrastructureProjectStatus::Requested,
            "under_review" => InfrastructureProjectStatus::UnderReview,
            "proposed" => InfrastructureProjectStatus::Proposed,
            "approved" => InfrastructureProjectStatus::Approved,
            "deferred" => InfrastructureProjectStatus::Deferred,
            "funding" => InfrastructureProjectStatus::Funding,
            "scheduled" => InfrastructureProjectStatus::Scheduled,
            "construction" => InfrastructureProjectStatus::Construction,
            "open" => InfrastructureProjectStatus::Open,
            "cancelled" => InfrastructureProjectStatus::Cancelled,
            _ => return Err(invalid_value(path, "Infrastructure Project status")),
        };
        let kind = match row.kind.as_str() {
            "new_line" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || line_targets.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "New Line project payload"));
                }
                InfrastructureProjectKind::NewLine {
                    planned_stations: planned_stations.remove(&row.id).unwrap_or_default(),
                    planned_lines: planned_lines.remove(&row.id).unwrap_or_default(),
                }
            }
            "speed_upgrade" => {
                if row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Speed Upgrade project payload"));
                }
                let target_speed_limit = SpeedKilometresPerHour::new(
                    row.target_speed_limit_kmh
                        .ok_or_else(|| invalid_value(path, "Speed Upgrade target speed"))?,
                )
                .map_err(|_| invalid_value(path, "Speed Upgrade target speed"))?;
                InfrastructureProjectKind::SpeedUpgrade {
                    rail_line_ids: line_targets.remove(&row.id).unwrap_or_default(),
                    target_speed_limit,
                }
            }
            "double_tracking" => {
                if row.target_speed_limit_kmh.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Double Tracking project payload"));
                }
                let target_track_count = TrackCount::new(
                    row.target_track_count
                        .ok_or_else(|| invalid_value(path, "Double Tracking target tracks"))?,
                )
                .map_err(|_| invalid_value(path, "Double Tracking target tracks"))?;
                InfrastructureProjectKind::DoubleTracking {
                    rail_line_ids: line_targets.remove(&row.id).unwrap_or_default(),
                    target_track_count,
                }
            }
            "electrification" | "renewal" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || station_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Rail Line project payload"));
                }
                let rail_line_ids = line_targets.remove(&row.id).unwrap_or_default();
                if row.kind == "electrification" {
                    InfrastructureProjectKind::Electrification { rail_line_ids }
                } else {
                    InfrastructureProjectKind::Renewal { rail_line_ids }
                }
            }
            "station_upgrade" => {
                if row.target_speed_limit_kmh.is_some()
                    || row.target_track_count.is_some()
                    || planned_stations.contains_key(&row.id)
                    || planned_lines.contains_key(&row.id)
                    || line_targets.contains_key(&row.id)
                {
                    return Err(invalid_value(path, "Station Upgrade project payload"));
                }
                InfrastructureProjectKind::StationUpgrade {
                    rail_station_ids: station_targets.remove(&row.id).unwrap_or_default(),
                }
            }
            _ => return Err(invalid_value(path, "Infrastructure Project kind")),
        };
        projects.push(InfrastructureProject {
            id: row.id,
            kind,
            status,
            timeline: row.timeline,
            funding: row.funding,
        });
    }

    if !line_targets.is_empty()
        || !station_targets.is_empty()
        || !planned_stations.is_empty()
        || !planned_lines.is_empty()
    {
        return Err(invalid_value(path, "orphan infrastructure project payload"));
    }

    Ok(projects)
}

pub(super) fn load_state(connection: &Connection, path: &Path) -> Result<Option<GameState>, SaveSlotError> {
    let meta = connection
        .query_row(
            "SELECT world_seed, last_processed_at FROM game_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|source| db_error("read game metadata from", path, source))?;
    let Some((world_seed_text, last_processed_at)) = meta else {
        return Ok(None);
    };
    let world_seed = world_seed_text
        .parse::<u64>()
        .map_err(|_| invalid_value(path, "world seed"))?;

    let (
        region_name,
        registration_code,
        registration_mark,
        region_population,
        authority_name,
        authority_construction_capacity,
    ): (String, i64, String, i64, String, i64) = connection
        .query_row(
            "SELECT name, registration_code, registration_mark, population, rail_authority_name,
                    rail_authority_construction_capacity
             FROM region WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|source| db_error("read Region from", path, source))?;

    let bulletin = query_all(
        connection,
        "SELECT occurred_at, category, headline, detail FROM bulletin_entries ORDER BY sequence",
        path,
        |row| {
            let category = match row.get::<_, String>(1)?.as_str() {
                "local" => BulletinCategory::Local,
                "authority" => BulletinCategory::Authority,
                "construction" => BulletinCategory::Construction,
                "network" => BulletinCategory::Network,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(BulletinEntry {
                occurred_at: UtcSeconds::from_unix_seconds(row.get(0)?),
                category,
                headline: row.get(2)?,
                detail: row.get(3)?,
            })
        },
    )?;

    let (
        authority_treasury,
        maintenance_reserve,
        committed_investment,
        carried_over_funds,
        regional_public_allocation,
        infrastructure_access_fee_revenue,
        next_fiscal_period_at,
    ): (i64, i64, i64, i64, i64, i64, Option<i64>) = connection
        .query_row(
            "SELECT treasury_cents, maintenance_reserve_cents, committed_investment_cents,
                    carried_over_funds_cents, regional_public_allocation_cents,
                    infrastructure_access_fee_revenue_cents, next_fiscal_period_at
             FROM rail_authority_finances WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(|source| db_error("read Rail Authority finances from", path, source))?;
    let rail_authority_finances = RailAuthorityFinances {
        treasury: Money::from_cents(authority_treasury),
        maintenance_reserve: Money::from_cents(maintenance_reserve),
        committed_investment: Money::from_cents(committed_investment),
        carried_over_funds: Money::from_cents(carried_over_funds),
        regional_public_allocation: Money::from_cents(regional_public_allocation),
        infrastructure_access_fee_revenue: Money::from_cents(infrastructure_access_fee_revenue),
        next_fiscal_period_at: next_fiscal_period_at.map(UtcSeconds::from_unix_seconds),
    };

    let settlements = query_all(
        connection,
        "SELECT id, name, population, world_x, world_y FROM settlements ORDER BY sequence",
        path,
        |row| {
            Ok(Settlement {
                id: row_domain_id(row, 0, "Settlement ID", SettlementId::parse)?,
                name: row.get(1)?,
                population: row_u64(row, 2, "Settlement Population")?,
                position: WorldPosition::new(row.get(3)?, row.get(4)?),
            })
        },
    )?;
    let rail_stations = query_all(
        connection,
        "SELECT id, settlement_id FROM rail_stations ORDER BY sequence",
        path,
        |row| {
            Ok(RailStation {
                id: row_domain_id(row, 0, "Rail Station ID", RailStationId::parse)?,
                settlement_id: row_domain_id(row, 1, "Settlement ID", SettlementId::parse)?,
            })
        },
    )?;
    let rail_lines = query_all(
        connection,
        "SELECT id, first_station_id, second_station_id, distance_metres,
                speed_limit_kmh, track_count, electrification, construction_difficulty
         FROM rail_lines ORDER BY sequence",
        path,
        |row| {
            let electrification = match row.get::<_, String>(6)?.as_str() {
                "none" => Electrification::None,
                "electric" => Electrification::Electric,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let construction_difficulty = match row.get::<_, String>(7)?.as_str() {
                "low" => ConstructionDifficulty::Low,
                "moderate" => ConstructionDifficulty::Moderate,
                "high" => ConstructionDifficulty::High,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(RailLine {
                id: row_domain_id(row, 0, "Rail Line ID", RailLineId::parse)?,
                first_station_id: row_domain_id(row, 1, "Rail Station ID", RailStationId::parse)?,
                second_station_id: row_domain_id(row, 2, "Rail Station ID", RailStationId::parse)?,
                distance: DistanceMetres::new(row.get(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                speed_limit: SpeedKilometresPerHour::new(row.get(4)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                track_count: TrackCount::new(row.get(5)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                electrification,
                construction_difficulty,
            })
        },
    )?;

    let infrastructure_projects = load_infrastructure_projects(connection, path)?;

    let (company_name, company_vkm, company_funds, next_train_display_number):
        (String, String, i64, i64) = connection
        .query_row(
            "SELECT name, vkm, funds_cents, next_train_display_number FROM company WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
            .map_err(|source| db_error("read Player Company from", path, source))?;
    let company_vkm = VehicleKeeperMark::parse(&company_vkm)
        .map_err(|_| invalid_value(path, "Player Company VKM"))?;

    let next_evn_unit_by_model = query_all(
        connection,
        "SELECT model_id, next_unit_number FROM train_model_sequences ORDER BY model_id",
        path,
        |row| {
            let model_id = TrainModelId::new(row.get::<_, String>(0)?);
            let raw: i64 = row.get(1)?;
            let next_unit_number = u16::try_from(raw).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok((model_id, next_unit_number))
        },
    )?
    .into_iter()
    .collect();

    let trains = query_all(
        connection,
        "SELECT id, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents FROM trains ORDER BY sequence",
        path,
        |row| {
            let evn_text: String = row.get(1)?;
            let evn = EuropeanVehicleNumber::parse(&evn_text)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let nickname = row
                .get::<_, Option<String>>(2)?
                .map(|value| {
                    TrainNickname::parse(&value).map_err(|_| rusqlite::Error::InvalidQuery)
                })
                .transpose()?;
            let status_kind: String = row.get(3)?;
            let status_ref: String = row.get(4)?;
            let status = match status_kind.as_str() {
                "ready" => TrainStatus::Ready {
                    at: RailStationId::parse(&status_ref)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                "travelling" => TrainStatus::Travelling {
                    journey_id: JourneyId::parse(&status_ref)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(Train {
                id: row_domain_id(row, 0, "Train ID", TrainId::parse)?,
                evn,
                nickname,
                status,
                model_id: TrainModelId::new(row.get::<_, String>(5)?),
                original_purchase_price: Money::from_cents(row.get(6)?),
            })
        },
    )?;

    let mut services = query_all(
        connection,
        "SELECT id, name FROM passenger_services ORDER BY sequence",
        path,
        |row| {
            Ok(PassengerService {
                id: row_domain_id(row, 0, "Passenger Service ID", ServiceId::parse)?,
                name: row.get(1)?,
                stop_station_ids: Vec::new(),
                rail_line_ids: Vec::new(),
            })
        },
    )?;
    for service in &mut services {
        let mut stop_statement = connection
            .prepare("SELECT station_id FROM service_stops WHERE service_id = ?1 ORDER BY sequence")
            .map_err(|source| db_error("prepare Passenger Service stop query for", path, source))?;
        let stop_rows = stop_statement
            .query_map(params![service.id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| db_error("read Passenger Service stops from", path, source))?;
        for row in stop_rows {
            let station =
                row.map_err(|source| db_error("read Passenger Service stop from", path, source))?;
            service.stop_station_ids.push(
                RailStationId::parse(&station)
                    .map_err(|_| invalid_value(path, "Rail Station ID"))?,
            );
        }

        let mut statement = connection
            .prepare(
                "SELECT rail_line_id FROM service_lines WHERE service_id = ?1 ORDER BY sequence",
            )
            .map_err(|source| db_error("prepare Passenger Service path query for", path, source))?;
        let rows = statement
            .query_map(params![service.id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| db_error("read Passenger Service path from", path, source))?;
        for row in rows {
            let line =
                row.map_err(|source| db_error("read Passenger Service path from", path, source))?;
            service
                .rail_line_ids
                .push(RailLineId::parse(&line).map_err(|_| invalid_value(path, "Rail Line ID"))?);
        }
    }

    let demand = query_all(
        connection,
        "SELECT origin_station_id, destination_station_id, waiting_passengers, market_maturity_basis_points, passenger_arrival_rate_per_hour, fractional_passenger_seconds FROM origin_destination_demand ORDER BY sequence",
        path,
        |row| {
            Ok(OriginDestinationDemand {
                origin_station_id: row_domain_id(row, 0, "Demand origin", RailStationId::parse)?,
                destination_station_id: row_domain_id(
                    row,
                    1,
                    "Demand destination",
                    RailStationId::parse,
                )?,
                waiting_passengers: u32::try_from(row.get::<_, i64>(2)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                market_maturity: MarketMaturity::from_basis_points(row.get(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                passenger_arrival_rate_per_hour: PassengerArrivalRate::new(row.get(4)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                fractional_passenger_seconds: row_u64(
                    row,
                    5,
                    "Demand fractional passenger seconds",
                )?,
            })
        },
    )?;

    let mut active_journeys = query_all(
        connection,
        "SELECT id, service_id, train_id, origin_station_id, destination_station_id, passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index, departed_at, arrives_at FROM active_journeys ORDER BY sequence",
        path,
        |row| {
            Ok(Journey {
                id: row_domain_id(row, 0, "Journey ID", JourneyId::parse)?,
                service_id: row_domain_id(row, 1, "Passenger Service ID", ServiceId::parse)?,
                train_id: row_domain_id(row, 2, "Train ID", TrainId::parse)?,
                origin_station_id: row_domain_id(row, 3, "Journey origin", RailStationId::parse)?,
                destination_station_id: row_domain_id(
                    row,
                    4,
                    "Journey destination",
                    RailStationId::parse,
                )?,
                passengers_carried: u32::try_from(row.get::<_, i64>(5)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                fare: Money::from_cents(row.get(6)?),
                operating_revenue: Money::from_cents(row.get(7)?),
                credited_revenue: Money::from_cents(row.get(8)?),
                infrastructure_access_fee: Money::from_cents(row.get(9)?),
                fuel_cost: Money::from_cents(row.get(10)?),
                current_stop_index: usize::try_from(row.get::<_, i64>(11)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                passenger_groups: Vec::new(),
                departed_at: UtcSeconds::from_unix_seconds(row.get(12)?),
                arrives_at: UtcSeconds::from_unix_seconds(row.get(13)?),
            })
        },
    )?;
    for journey in &mut active_journeys {
        let mut statement = connection
            .prepare(
                "SELECT origin_station_id, destination_station_id, passengers, fare_cents
             FROM journey_passenger_groups
             WHERE journey_id = ?1
             ORDER BY sequence",
            )
            .map_err(|source| {
                db_error("prepare Journey passenger group query for", path, source)
            })?;
        let rows = statement
            .query_map(params![journey.id.to_string()], |row| {
                Ok(JourneyPassengerGroup {
                    origin_station_id: row_domain_id(
                        row,
                        0,
                        "Journey passenger origin",
                        RailStationId::parse,
                    )?,
                    destination_station_id: row_domain_id(
                        row,
                        1,
                        "Journey passenger destination",
                        RailStationId::parse,
                    )?,
                    passengers: u32::try_from(row.get::<_, i64>(2)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    fare: Money::from_cents(row.get(3)?),
                })
            })
            .map_err(|source| db_error("read Journey passenger groups from", path, source))?;
        for row in rows {
            journey.passenger_groups.push(
                row.map_err(|source| db_error("read Journey passenger group from", path, source))?,
            );
        }
    }

    let (operating_revenue, access_fees, fuel_costs): (i64, i64, i64) = connection
        .query_row("SELECT operating_revenue_cents, infrastructure_access_fees_cents, fuel_costs_cents FROM financials WHERE singleton = 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|source| db_error("read financial totals from", path, source))?;
    let receipt_sql = format!(
        "SELECT journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at
         FROM (
             SELECT journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents, train_id, train_model_name, origin_station_id, destination_station_id, passengers_carried, passenger_capacity, completed_at
             FROM journey_receipts
             ORDER BY completed_at DESC, journey_id DESC
             LIMIT {RECENT_RECEIPT_LIMIT}
         )
         ORDER BY completed_at, journey_id"
    );
    let receipts = query_all(connection, &receipt_sql, path, |row| {
        Ok(JourneyReceipt {
            journey_id: row_domain_id(row, 0, "Journey receipt ID", JourneyId::parse)?,
            revenue: Money::from_cents(row.get(1)?),
            infrastructure_access_fee: Money::from_cents(row.get(2)?),
            fuel_cost: Money::from_cents(row.get(3)?),
            train_id: optional_row_domain_id(row, 4, "Journey receipt Train ID", TrainId::parse)?,
            train_model_name: row.get(5)?,
            origin_station_id: optional_row_domain_id(
                row,
                6,
                "Journey receipt origin",
                RailStationId::parse,
            )?,
            destination_station_id: optional_row_domain_id(
                row,
                7,
                "Journey receipt destination",
                RailStationId::parse,
            )?,
            passengers_carried: optional_row_u32(row, 8, "Journey receipt passengers")?,
            passenger_capacity: optional_row_u32(row, 9, "Journey receipt passenger capacity")?,
            completed_at: row
                .get::<_, Option<i64>>(10)?
                .map(UtcSeconds::from_unix_seconds),
        })
    })?;

    let (fare_rate, access_rate, starting_funds, demand_cap): (i64, i64, i64, i64) = connection
        .query_row("SELECT fare_cents_per_passenger_km, access_fee_cents_per_train_km, starting_company_funds_cents, demand_cap_seconds FROM game_rules WHERE singleton = 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|source| db_error("read game rules from", path, source))?;

    let state = GameState {
        world_seed,
        region: Region {
            name: region_name,
            railway_registration: RailwayRegistration {
                numeric_code: u8::try_from(registration_code)
                    .map_err(|_| invalid_value(path, "Region railway registration code"))?,
                mark: registration_mark,
            },
            population: from_db_u64(region_population, "Region Population")
                .map_err(|field| invalid_value(path, field))?,
            settlements,
            bulletin,
            rail_authority: RailAuthority {
                name: authority_name,
                rail_network: RailNetwork {
                    rail_stations,
                    rail_lines,
                },
                finances: rail_authority_finances,
                construction_capacity: u32::try_from(authority_construction_capacity)
                    .map_err(|_| invalid_value(path, "Rail Authority construction capacity"))?,
                infrastructure_projects,
            },
        },
        player_company: PlayerCompany {
            name: company_name,
            vehicle_keeper_mark: company_vkm,
            funds: Money::from_cents(company_funds),
            fleet: Fleet {
                trains,
                next_train_display_number: from_db_u64(
                    next_train_display_number,
                    "next Train display number",
                )
                .map_err(|field| invalid_value(path, field))?,
                next_evn_unit_by_model,
            },
            passenger_services: services,
        },
        origin_destination_demand: demand,
        active_journeys,
        financials: Financials {
            operating_revenue: Money::from_cents(operating_revenue),
            infrastructure_access_fees: Money::from_cents(access_fees),
            fuel_costs: Money::from_cents(fuel_costs),
            recent_journey_receipts: receipts,
        },
        rules: GameRules {
            balance: BalanceConfig::new(
                MoneyPerKilometre::new(fare_rate).map_err(|_| invalid_value(path, "fare rate"))?,
                MoneyPerKilometre::new(access_rate)
                    .map_err(|_| invalid_value(path, "access fee rate"))?,
                Money::from_cents(starting_funds),
            ),
            demand: DemandRules {
                cap_duration: DurationSeconds::from_seconds(
                    from_db_u64(demand_cap, "demand cap duration")
                        .map_err(|field| invalid_value(path, field))?,
                ),
            },
        },
        last_processed_at: UtcSeconds::from_unix_seconds(last_processed_at),
    };
    Ok(Some(state))
}


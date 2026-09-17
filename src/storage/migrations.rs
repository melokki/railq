use super::*;
use rusqlite::OptionalExtension;

pub(super) fn ensure_schema(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    let mut version = read_schema_version(connection, path, "read schema version from")?;

    if version == 0 {
        connection
            .execute_batch(SCHEMA)
            .map_err(|source| SaveSlotError::Database {
                action: "initialize schema for",
                path: path.to_path_buf(),
                source,
            })?;
        connection
            .pragma_update(None, "user_version", SAVE_VERSION)
            .map_err(|source| SaveSlotError::Database {
                action: "write schema version to",
                path: path.to_path_buf(),
                source,
            })?;
        return Ok(());
    }

    if version > SAVE_VERSION {
        return Err(unsupported_version(path, version));
    }

    if version == SAVE_VERSION {
        connection
            .execute_batch(SCHEMA)
            .map_err(|source| SaveSlotError::Database {
                action: "verify schema for",
                path: path.to_path_buf(),
                source,
            })?;
        return Ok(());
    }

    while version < SAVE_VERSION {
        migrate_one_version(connection, path, version)?;
        version = read_schema_version(connection, path, "read migrated schema version from")?;
    }

    Ok(())
}

fn migrate_one_version(
    connection: &Connection,
    path: &Path,
    version: u32,
) -> Result<(), SaveSlotError> {
    match version {
        1 => migrate_v1_to_v2(connection, path),
        2 => migrate_v2_to_v3(connection, path),
        3 => migrate_v3_to_v4(connection, path),
        4 => migrate_v4_to_v5(connection, path),
        5 => migrate_v5_to_v6(connection, path),
        6 => migrate_v6_to_v7(connection, path),
        7 => migrate_v7_to_v8(connection, path),
        8 => migrate_v8_to_v9(connection, path),
        9 => migrate_v9_to_v10(connection, path),
        10 => migrate_v10_to_v11(connection, path),
        11 => migrate_v11_to_v12(connection, path),
        12 => migrate_v12_to_v13(connection, path),
        13 => migrate_v13_to_v14(connection, path),
        14 => migrate_v14_to_v15(connection, path),
        15 => migrate_v15_to_v16(connection, path),
        16 => migrate_v16_to_v17(connection, path),
        17 => migrate_v17_to_v18(connection, path),
        18 => migrate_v18_to_v19(connection, path),
        19 => migrate_v19_to_v20(connection, path),
        20 => migrate_v20_to_v21(connection, path),
        21 => migrate_v21_to_v22(connection, path),
        22 => migrate_v22_to_v23(connection, path),
        23 => migrate_v23_to_v24(connection, path),
        24 => migrate_v24_to_v25(connection, path),
        25 => migrate_v25_to_v26(connection, path),
        26 => migrate_v26_to_v27(connection, path),
        27 => migrate_v27_to_v28(connection, path),
        28 => migrate_v28_to_v29(connection, path),
        29 => migrate_v29_to_v30(connection, path),
        30 => migrate_v30_to_v31(connection, path),
        31 => migrate_v31_to_v32(connection, path),
        found => Err(unsupported_version(path, found)),
    }
}

fn read_schema_version(
    connection: &Connection,
    path: &Path,
    action: &'static str,
) -> Result<u32, SaveSlotError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| db_error(action, path, source))
}

fn unsupported_version(path: &Path, found: u32) -> SaveSlotError {
    SaveSlotError::InvalidSave {
        path: path.to_path_buf(),
        source: Box::new(SaveCodecError::UnsupportedVersion { found }),
    }
}

fn migrate_v1_to_v2(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE trains RENAME TO trains_v1;
             CREATE TABLE trains (
                 id INTEGER PRIMARY KEY,
                 status_kind TEXT NOT NULL CHECK (status_kind IN ('ready', 'travelling')),
                 status_ref_id TEXT NOT NULL,
                 model_id TEXT NOT NULL,
                 original_purchase_price_cents INTEGER NOT NULL
             );",
        )
        .map_err(|source| db_error("begin v1 to v2 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let mut statement = connection
            .prepare(
                "SELECT id, status_kind, status_ref_id, model_name, original_purchase_price_cents,
                        passenger_capacity, speed_metres_per_second, fuel_cost_cents_per_km
                 FROM trains_v1 ORDER BY id",
            )
            .map_err(|source| db_error("read v1 Trains from", path, source))?;
        let mut rows = statement
            .query([])
            .map_err(|source| db_error("read v1 Trains from", path, source))?;
        while let Some(row) = rows
            .next()
            .map_err(|source| db_error("read v1 Train row from", path, source))?
        {
            let model_name: String = row
                .get(3)
                .map_err(|source| db_error("decode v1 Train model from", path, source))?;
            let passenger_capacity: i64 = row
                .get(5)
                .map_err(|source| db_error("decode v1 Train capacity from", path, source))?;
            let speed: i64 = row
                .get(6)
                .map_err(|source| db_error("decode v1 Train speed from", path, source))?;
            let fuel_rate: i64 = row
                .get(7)
                .map_err(|source| db_error("decode v1 Train fuel rate from", path, source))?;
            let model = catalogue_model_for_legacy_signature(
                &model_name,
                passenger_capacity,
                speed,
                fuel_rate,
            )
            .ok_or_else(|| SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::TrainModelNotFound {
                    model_name: model_name.clone(),
                }),
            })?;

            connection
                .execute(
                    "INSERT INTO trains(id, status_kind, status_ref_id, model_id, original_purchase_price_cents)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        row.get::<_, i64>(0).map_err(|source| db_error("decode v1 Train ID from", path, source))?,
                        row.get::<_, String>(1).map_err(|source| db_error("decode v1 Train status from", path, source))?,
                        row.get::<_, i64>(2).map_err(|source| db_error("decode v1 Train status reference from", path, source))?,
                        model.id().as_str(),
                        row.get::<_, i64>(4).map_err(|source| db_error("decode v1 Train price from", path, source))?,
                    ],
                )
                .map_err(|source| db_error("write migrated Train to", path, source))?;
        }
        drop(rows);
        drop(statement);
        connection
            .execute_batch(
                "DROP TABLE trains_v1;
                 DROP TABLE IF EXISTS diesel_catalogue;",
            )
            .map_err(|source| db_error("finish v1 to v2 schema migration for", path, source))?;
        connection
            .pragma_update(None, "user_version", 2_u32)
            .map_err(|source| db_error("write v2 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("verify v1 to v2 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v1 to v2 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA legacy_alter_table = OFF;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v1 to v2 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA legacy_alter_table = OFF;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v2_to_v3(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE active_journeys RENAME TO active_journeys_v2;
             ALTER TABLE service_lines RENAME TO service_lines_v2;
             ALTER TABLE passenger_services RENAME TO passenger_services_v2;
             CREATE TABLE passenger_services (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE service_stops (
                 service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 station_id TEXT NOT NULL REFERENCES rail_stations(id),
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE service_lines (
                 service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
                 PRIMARY KEY (service_id, sequence)
             );
             CREATE TABLE active_journeys (
                 id INTEGER PRIMARY KEY,
                 service_id INTEGER NOT NULL REFERENCES passenger_services(id),
                 train_id INTEGER NOT NULL REFERENCES trains(id),
                 origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 passengers_carried INTEGER NOT NULL,
                 fare_cents INTEGER NOT NULL,
                 operating_revenue_cents INTEGER NOT NULL,
                 infrastructure_access_fee_cents INTEGER NOT NULL,
                 fuel_cost_cents INTEGER NOT NULL,
                 departed_at INTEGER NOT NULL,
                 arrives_at INTEGER NOT NULL
             );
             INSERT INTO passenger_services(id, name)
                 SELECT id, 'R' || id FROM passenger_services_v2;
             INSERT INTO service_stops(service_id, sequence, station_id)
                 SELECT id, 0, first_station_id FROM passenger_services_v2;
             INSERT INTO service_stops(service_id, sequence, station_id)
                 SELECT id, 1, second_station_id FROM passenger_services_v2;
             INSERT INTO service_lines(service_id, sequence, rail_line_id)
                 SELECT service_id, sequence, rail_line_id FROM service_lines_v2;
             INSERT INTO active_journeys(
                 id, service_id, train_id, origin_station_id, destination_station_id,
                 passengers_carried, fare_cents, operating_revenue_cents,
                 infrastructure_access_fee_cents, fuel_cost_cents, departed_at, arrives_at
             )
                 SELECT id, service_id, train_id, origin_station_id, destination_station_id,
                        passengers_carried, fare_cents, operating_revenue_cents,
                        infrastructure_access_fee_cents, fuel_cost_cents, departed_at, arrives_at
                 FROM active_journeys_v2;
             DROP TABLE active_journeys_v2;
             DROP TABLE service_lines_v2;
             DROP TABLE passenger_services_v2;
             CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);",
        )
        .map_err(|source| db_error("begin v2 to v3 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .pragma_update(None, "user_version", 3_u32)
            .map_err(|source| db_error("write v3 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("verify v2 to v3 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v2 to v3 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA legacy_alter_table = OFF;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v2 to v3 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA legacy_alter_table = OFF;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v3_to_v4(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             BEGIN IMMEDIATE;
             ALTER TABLE active_journeys ADD COLUMN credited_revenue_cents INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE active_journeys ADD COLUMN current_stop_index INTEGER NOT NULL DEFAULT 0;
             CREATE TABLE journey_passenger_groups (
                 journey_id TEXT NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 origin_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 destination_station_id INTEGER NOT NULL REFERENCES rail_stations(id),
                 passengers INTEGER NOT NULL,
                 fare_cents INTEGER NOT NULL,
                 PRIMARY KEY (journey_id, sequence)
             );
             INSERT INTO journey_passenger_groups(
                 journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents
             )
                 SELECT id, 0, origin_station_id, destination_station_id, passengers_carried, fare_cents
                 FROM active_journeys
                 WHERE passengers_carried > 0;
             UPDATE active_journeys
             SET current_stop_index = CASE
                 WHEN origin_station_id = (
                     SELECT station_id FROM service_stops
                     WHERE service_id = active_journeys.service_id
                     ORDER BY sequence ASC LIMIT 1
                 ) THEN MAX((
                     SELECT COUNT(*) FROM service_stops
                     WHERE service_id = active_journeys.service_id
                 ) - 2, 0)
                 WHEN origin_station_id = (
                     SELECT station_id FROM service_stops
                     WHERE service_id = active_journeys.service_id
                     ORDER BY sequence DESC LIMIT 1
                 ) THEN MIN(1, (
                     SELECT COUNT(*) FROM service_stops
                     WHERE service_id = active_journeys.service_id
                 ) - 1)
                 ELSE 0
             END;",
        )
        .map_err(|source| db_error("begin v3 to v4 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .pragma_update(None, "user_version", 4_u32)
            .map_err(|source| db_error("write v4 schema version to", path, source))?;
        let foreign_key_violation: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("verify v3 to v4 migration for", path, source))?;
        if foreign_key_violation.is_some() {
            return Err(SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "foreign keys after v3 to v4 migration",
                }),
            });
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit v3 to v4 migration for", path, source))?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v4_to_v5(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE region ADD COLUMN registration_code INTEGER NOT NULL DEFAULT 99
                 CHECK (registration_code BETWEEN 10 AND 99);
             ALTER TABLE region ADD COLUMN registration_mark TEXT NOT NULL DEFAULT 'RQ';",
        )
        .map_err(|source| db_error("begin v4 to v5 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT r.name, g.world_seed
                 FROM region r
                 CROSS JOIN game_meta g
                 WHERE r.singleton = 1 AND g.singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| {
                db_error("read Region registration migration data from", path, source)
            })?;

        if let Some((region_name, world_seed_text)) = existing {
            let world_seed = world_seed_text
                .parse::<u64>()
                .map_err(|_| invalid_value(path, "world seed"))?;
            let registration = railway_registration_for_existing_region(&region_name, world_seed);
            connection
                .execute(
                    "UPDATE region
                     SET registration_code = ?1, registration_mark = ?2
                     WHERE singleton = 1",
                    params![i64::from(registration.numeric_code), registration.mark],
                )
                .map_err(|source| {
                    db_error("write Region registration identity to", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 5_u32)
            .map_err(|source| db_error("write v5 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v4 to v5 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v5_to_v6(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE company ADD COLUMN vkm TEXT NOT NULL DEFAULT 'RQ'
                 CHECK (length(vkm) BETWEEN 2 AND 5)
                 CHECK (vkm NOT GLOB '*[^A-Z]*');",
        )
        .map_err(|source| db_error("begin v5 to v6 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let company_name: Option<String> = connection
            .query_row("SELECT name FROM company WHERE singleton = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|source| db_error("read Company VKM migration data from", path, source))?;

        if let Some(company_name) = company_name {
            let vkm = VehicleKeeperMark::generated_from_company_name(&company_name);
            connection
                .execute(
                    "UPDATE company SET vkm = ?1 WHERE singleton = 1",
                    params![vkm.as_str()],
                )
                .map_err(|source| db_error("write Company VKM to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 6_u32)
            .map_err(|source| db_error("write v6 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v5 to v6 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v6_to_v7(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE company ADD COLUMN next_train_id INTEGER NOT NULL DEFAULT 1
                 CHECK (next_train_id > 0);
             ALTER TABLE trains ADD COLUMN evn TEXT;",
        )
        .map_err(|source| db_error("begin v6 to v7 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let registration_code: Option<i64> = connection
            .query_row(
                "SELECT registration_code FROM region WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| db_error("read EVN Region registration from", path, source))?;

        let next_train_id: i64 = connection
            .query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM trains", [], |row| {
                row.get(0)
            })
            .map_err(|source| {
                db_error(
                    "calculate next Train ID for v7 migration from",
                    path,
                    source,
                )
            })?;
        connection
            .execute(
                "UPDATE company SET next_train_id = ?1 WHERE singleton = 1",
                params![next_train_id],
            )
            .map_err(|source| db_error("write next Train ID to", path, source))?;

        if let Some(registration_code) = registration_code {
            let registration_code = u8::try_from(registration_code)
                .map_err(|_| invalid_value(path, "Region registration code"))?;
            let mut statement = connection
                .prepare("SELECT id, model_id FROM trains ORDER BY model_id, id")
                .map_err(|source| {
                    db_error("read v6 Trains for EVN migration from", path, source)
                })?;
            let mut rows = statement.query([]).map_err(|source| {
                db_error("read v6 Trains for EVN migration from", path, source)
            })?;

            let mut migrated = Vec::new();
            let mut next_units: HashMap<String, u16> = HashMap::new();
            while let Some(row) = rows.next().map_err(|source| {
                db_error("read v6 Train row for EVN migration from", path, source)
            })? {
                let train_id = from_db_u64(
                    row.get::<_, i64>(0)
                        .map_err(|source| db_error("decode v6 Train ID from", path, source))?,
                    "Train ID",
                )
                .map_err(|field| invalid_value(path, field))?;
                let model_id: String = row
                    .get(1)
                    .map_err(|source| db_error("decode v6 Train model from", path, source))?;
                let model = catalogue_model_for_persisted_id(&model_id).ok_or_else(|| {
                    SaveSlotError::InvalidSave {
                        path: path.to_path_buf(),
                        source: Box::new(SaveCodecError::TrainModelNotFound {
                            model_name: model_id.clone(),
                        }),
                    }
                })?;
                let unit_number = next_units.entry(model_id.clone()).or_insert(1);
                if *unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER {
                    return Err(invalid_value(path, "EVN unit number"));
                }
                let evn = EuropeanVehicleNumber::generate(
                    model.evn_type_code(),
                    registration_code,
                    model.evn_series_code(),
                    *unit_number,
                )
                .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
                migrated.push((train_id, evn));
                *unit_number += 1;
            }
            drop(rows);
            drop(statement);

            for (train_id, evn) in migrated {
                connection
                    .execute(
                        "UPDATE trains SET evn = ?1 WHERE id = ?2",
                        params![evn.as_str(), to_db_u64(train_id, "Train ID", path)?],
                    )
                    .map_err(|source| db_error("write migrated Train EVN to", path, source))?;
            }
        }

        let missing_evn: i64 = connection
            .query_row("SELECT COUNT(*) FROM trains WHERE evn IS NULL", [], |row| {
                row.get(0)
            })
            .map_err(|source| db_error("verify migrated Train EVNs in", path, source))?;
        if missing_evn != 0 {
            return Err(invalid_value(path, "European Vehicle Number"));
        }

        connection
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_trains_evn ON trains(evn)
                 WHERE evn IS NOT NULL;",
            )
            .map_err(|source| db_error("index migrated Train EVNs in", path, source))?;
        connection
            .pragma_update(None, "user_version", 7_u32)
            .map_err(|source| db_error("write v7 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v6 to v7 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v7_to_v8(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE train_model_sequences (
                 model_id TEXT PRIMARY KEY,
                 next_unit_number INTEGER NOT NULL CHECK (next_unit_number BETWEEN 1 AND 1000)
             );",
        )
        .map_err(|source| db_error("begin v7 to v8 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let registration_code: i64 = connection
            .query_row(
                "SELECT registration_code FROM region WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error("read Region registration for v8 EVNs from", path, source)
            })?;
        let registration_code = u8::try_from(registration_code)
            .map_err(|_| invalid_value(path, "Region registration code"))?;

        let mut statement = connection
            .prepare("SELECT id, model_id FROM trains ORDER BY model_id, id")
            .map_err(|source| db_error("read v7 Trains for EVN refinement from", path, source))?;
        let mut rows = statement
            .query([])
            .map_err(|source| db_error("read v7 Trains for EVN refinement from", path, source))?;

        let mut next_units: HashMap<String, u16> = HashMap::new();
        let mut migrated = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|source| db_error("read v7 Train row for EVN refinement from", path, source))?
        {
            let train_id = from_db_u64(
                row.get::<_, i64>(0)
                    .map_err(|source| db_error("decode v7 Train ID from", path, source))?,
                "Train ID",
            )
            .map_err(|field| invalid_value(path, field))?;
            let model_id: String = row
                .get(1)
                .map_err(|source| db_error("decode v7 Train model from", path, source))?;
            let model = catalogue_model_for_persisted_id(&model_id).ok_or_else(|| {
                SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::TrainModelNotFound {
                        model_name: model_id.clone(),
                    }),
                }
            })?;

            let unit_number = next_units.entry(model_id.clone()).or_insert(1);
            if *unit_number > EuropeanVehicleNumber::MAX_UNIT_NUMBER {
                return Err(invalid_value(path, "EVN unit number"));
            }
            let evn = EuropeanVehicleNumber::generate(
                model.evn_type_code(),
                registration_code,
                model.evn_series_code(),
                *unit_number,
            )
            .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            migrated.push((train_id, evn));
            *unit_number += 1;
        }
        drop(rows);
        drop(statement);

        for (train_id, evn) in migrated {
            connection
                .execute(
                    "UPDATE trains SET evn = ?1 WHERE id = ?2",
                    params![evn.as_str(), to_db_u64(train_id, "Train ID", path)?],
                )
                .map_err(|source| db_error("write refined Train EVN to", path, source))?;
        }

        for (model_id, next_unit_number) in next_units {
            connection
                .execute(
                    "INSERT INTO train_model_sequences(model_id, next_unit_number) VALUES(?1, ?2)",
                    params![model_id, i64::from(next_unit_number)],
                )
                .map_err(|source| db_error("write EVN model sequence to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 8_u32)
            .map_err(|source| db_error("write v8 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v7 to v8 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v8_to_v9(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE trains ADD COLUMN nickname TEXT
                 CHECK (nickname IS NULL OR length(trim(nickname)) BETWEEN 1 AND 32);",
        )
        .map_err(|source| db_error("begin v8 to v9 migration for", path, source))?;

    let migration = connection
        .pragma_update(None, "user_version", 9_u32)
        .map_err(|source| db_error("write v9 schema version to", path, source));

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v8 to v9 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

pub(super) fn migrate_v9_to_v10(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v9 to v10 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let mut statement = connection
            .prepare(
                "SELECT CAST(id AS TEXT), evn, model_id
                 FROM trains
                 WHERE model_id IN ('local-70', 'express-120')
                 ORDER BY id",
            )
            .map_err(|source| {
                db_error("read v9 Trains for catalogue migration from", path, source)
            })?;
        let mut rows = statement.query([]).map_err(|source| {
            db_error("read v9 Trains for catalogue migration from", path, source)
        })?;
        let mut migrated_trains = Vec::new();
        while let Some(row) = rows.next().map_err(|source| {
            db_error(
                "read v9 Train row for catalogue migration from",
                path,
                source,
            )
        })? {
            let train_id: String = row
                .get(0)
                .map_err(|source| db_error("decode v9 Train ID from", path, source))?;
            let old_evn: String = row
                .get(1)
                .map_err(|source| db_error("decode v9 Train EVN from", path, source))?;
            let old_model_id: String = row
                .get(2)
                .map_err(|source| db_error("decode v9 Train model from", path, source))?;
            let replacement = catalogue_model_for_persisted_id(&old_model_id).ok_or_else(|| {
                SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::TrainModelNotFound {
                        model_name: old_model_id.clone(),
                    }),
                }
            })?;
            let old_evn = EuropeanVehicleNumber::parse(&old_evn)
                .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            let replacement_evn = EuropeanVehicleNumber::generate(
                replacement.evn_type_code(),
                old_evn.registration_code(),
                replacement.evn_series_code(),
                old_evn.unit_number(),
            )
            .map_err(|_| invalid_value(path, "European Vehicle Number"))?;
            migrated_trains.push((
                train_id,
                replacement_evn,
                replacement.id().as_str().to_owned(),
            ));
        }
        drop(rows);
        drop(statement);

        for (train_id, evn, model_id) in migrated_trains {
            connection
                .execute(
                    "UPDATE trains SET evn = ?1, model_id = ?2 WHERE CAST(id AS TEXT) = ?3",
                    params![evn.as_str(), model_id, train_id],
                )
                .map_err(|source| db_error("write migrated v10 Train to", path, source))?;
        }

        for (old_model_id, new_model_id) in [
            ("local-70", "helvetra-r70"),
            ("express-120", "veltrian-d121"),
        ] {
            let old_next_unit: Option<i64> = connection
                .query_row(
                    "SELECT next_unit_number FROM train_model_sequences WHERE model_id = ?1",
                    params![old_model_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("read legacy EVN sequence from", path, source))?;
            let Some(old_next_unit) = old_next_unit else {
                continue;
            };
            let new_next_unit: Option<i64> = connection
                .query_row(
                    "SELECT next_unit_number FROM train_model_sequences WHERE model_id = ?1",
                    params![new_model_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("read replacement EVN sequence from", path, source))?;
            let merged_next_unit =
                new_next_unit.map_or(old_next_unit, |current| current.max(old_next_unit));

            connection
                .execute(
                    "DELETE FROM train_model_sequences WHERE model_id = ?1",
                    params![old_model_id],
                )
                .map_err(|source| db_error("remove legacy EVN sequence from", path, source))?;
            connection
                .execute(
                    "INSERT INTO train_model_sequences(model_id, next_unit_number)
                     VALUES(?1, ?2)
                     ON CONFLICT(model_id) DO UPDATE SET next_unit_number = excluded.next_unit_number",
                    params![new_model_id, merged_next_unit],
                )
                .map_err(|source| db_error("write replacement EVN sequence to", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 10_u32)
            .map_err(|source| db_error("write v10 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v9 to v10 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v10_to_v11(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v10 to v11 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE rail_lines ADD COLUMN speed_limit_kmh INTEGER NOT NULL DEFAULT 70 CHECK (speed_limit_kmh > 0);
                 ALTER TABLE rail_lines ADD COLUMN track_count INTEGER NOT NULL DEFAULT 1 CHECK (track_count > 0);
                 ALTER TABLE rail_lines ADD COLUMN electrification TEXT NOT NULL DEFAULT 'none' CHECK (electrification IN ('none', 'electric'));
                 ALTER TABLE rail_lines ADD COLUMN construction_difficulty TEXT NOT NULL DEFAULT 'moderate' CHECK (construction_difficulty IN ('low', 'moderate', 'high'));",
            )
            .map_err(|source| db_error("add Rail Line capabilities to", path, source))?;
        connection
            .pragma_update(None, "user_version", 11_u32)
            .map_err(|source| db_error("write v11 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v10 to v11 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v11_to_v12(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v11 to v12 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE region ADD COLUMN next_infrastructure_project_id INTEGER NOT NULL DEFAULT 1 CHECK (next_infrastructure_project_id > 0);
                 CREATE TABLE infrastructure_projects (
                     id INTEGER PRIMARY KEY,
                     kind TEXT NOT NULL CHECK (kind IN ('new_line', 'speed_upgrade', 'double_tracking', 'electrification', 'renewal', 'station_upgrade')),
                     status TEXT NOT NULL CHECK (status IN ('requested', 'under_review', 'proposed', 'approved', 'deferred', 'funding', 'scheduled', 'construction', 'open', 'cancelled')),
                     requested_at INTEGER NOT NULL,
                     review_started_at INTEGER,
                     proposed_at INTEGER,
                     approved_at INTEGER,
                     funding_completed_at INTEGER,
                     scheduled_start_at INTEGER,
                     construction_started_at INTEGER,
                     planned_completion_at INTEGER,
                     completed_at INTEGER,
                     deferred_at INTEGER,
                     cancelled_at INTEGER,
                     target_speed_limit_kmh INTEGER CHECK (target_speed_limit_kmh IS NULL OR target_speed_limit_kmh > 0),
                     target_track_count INTEGER CHECK (target_track_count IS NULL OR target_track_count > 0)
                 );
                 CREATE TABLE infrastructure_project_rail_lines (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_rail_stations (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     rail_station_id TEXT NOT NULL REFERENCES rail_stations(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_planned_stations (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     station_id INTEGER NOT NULL UNIQUE,
                     settlement_id INTEGER NOT NULL REFERENCES settlements(id),
                     PRIMARY KEY (project_id, sequence)
                 );
                 CREATE TABLE infrastructure_project_planned_lines (
                     project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
                     sequence INTEGER NOT NULL,
                     line_id INTEGER NOT NULL UNIQUE,
                     first_station_id INTEGER NOT NULL,
                     second_station_id INTEGER NOT NULL,
                     distance_metres INTEGER NOT NULL CHECK (distance_metres > 0),
                     speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
                     track_count INTEGER NOT NULL CHECK (track_count > 0),
                     electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
                     construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high')),
                     PRIMARY KEY (project_id, sequence)
                 );",
            )
            .map_err(|source| db_error("add infrastructure project persistence to", path, source))?;
        connection
            .pragma_update(None, "user_version", 12_u32)
            .map_err(|source| db_error("write v12 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v11 to v12 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v12_to_v13(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v12 to v13 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "CREATE TABLE rail_authority_finances (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
                     maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
                     committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
                     carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0)
                 );
                 INSERT INTO rail_authority_finances(
                     singleton, treasury_cents, maintenance_reserve_cents,
                     committed_investment_cents, carried_over_funds_cents
                 ) VALUES(1, 0, 0, 0, 0);",
            )
            .map_err(|source| db_error("add Rail Authority finances to", path, source))?;
        connection
            .pragma_update(None, "user_version", 13_u32)
            .map_err(|source| db_error("write v13 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v12 to v13 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v13_to_v14(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v13 to v14 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let allocation = crate::model::PROVISIONAL_REGIONAL_PUBLIC_ALLOCATION.cents();
        connection
            .execute(
                "ALTER TABLE rail_authority_finances
                 ADD COLUMN regional_public_allocation_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (regional_public_allocation_cents >= 0)",
                [],
            )
            .map_err(|source| db_error("add regional public allocation to", path, source))?;
        connection
            .execute(
                "UPDATE rail_authority_finances
                 SET regional_public_allocation_cents = ?1,
                     treasury_cents = treasury_cents + ?1
                 WHERE singleton = 1",
                params![allocation],
            )
            .map_err(|source| db_error("seed regional public allocation in", path, source))?;
        connection
            .pragma_update(None, "user_version", 14_u32)
            .map_err(|source| db_error("write v14 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v13 to v14 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v14_to_v15(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v14 to v15 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let rail_lines_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_lines'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error("inspect Rail Lines during v15 migration in", path, source)
            })?;

        if rail_lines_exist != 0 {
            let rate = crate::model::PROVISIONAL_MAINTENANCE_RESERVE_PER_TRACK_KILOMETRE
                .cents_per_kilometre();
            let rate = i64::try_from(rate)
                .expect("the provisional maintenance reserve rate fits SQLite INTEGER");
            connection
                .execute(
                    "UPDATE rail_authority_finances
                     SET maintenance_reserve_cents = MIN(
                         treasury_cents - committed_investment_cents,
                         COALESCE((
                             SELECT SUM((((distance_metres * ?1) + 999) / 1000) * track_count)
                             FROM rail_lines
                         ), 0)
                     )
                     WHERE singleton = 1",
                    params![rate],
                )
                .map_err(|source| {
                    db_error("seed infrastructure maintenance reserve in", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 15_u32)
            .map_err(|source| db_error("write v15 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v14 to v15 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v15_to_v16(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v15 to v16 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE rail_authority_finances
                 ADD COLUMN infrastructure_access_fee_revenue_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (infrastructure_access_fee_revenue_cents >= 0);",
            )
            .map_err(|source| db_error("add Authority access-fee revenue to", path, source))?;

        let financials_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'financials'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect financial history during v16 migration in",
                    path,
                    source,
                )
            })?;
        if financials_exist != 0 {
            let historical_access_fees: i64 = connection
                .query_row(
                    "SELECT COALESCE(infrastructure_access_fees_cents, 0)
                     FROM financials WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| {
                    db_error(
                        "read historical access fees during v16 migration from",
                        path,
                        source,
                    )
                })?
                .unwrap_or(0);
            connection
                .execute(
                    "UPDATE rail_authority_finances
                     SET infrastructure_access_fee_revenue_cents = ?1,
                         treasury_cents = treasury_cents + ?1
                     WHERE singleton = 1",
                    params![historical_access_fees],
                )
                .map_err(|source| {
                    db_error(
                        "credit historical access fees to Rail Authority in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 16_u32)
            .map_err(|source| db_error("write v16 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v15 to v16 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v16_to_v17(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch(V16_IDENTITY_TABLES_COMPAT_SCHEMA)
        .map_err(|source| db_error("prepare v16 UUID migration tables in", path, source))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN IMMEDIATE;
             ALTER TABLE region RENAME TO region_v16;
             ALTER TABLE settlements RENAME TO settlements_v16;
             ALTER TABLE rail_stations RENAME TO rail_stations_v16;
             ALTER TABLE rail_lines RENAME TO rail_lines_v16;
             ALTER TABLE infrastructure_projects RENAME TO infrastructure_projects_v16;
             ALTER TABLE infrastructure_project_rail_lines RENAME TO infrastructure_project_rail_lines_v16;
             ALTER TABLE infrastructure_project_rail_stations RENAME TO infrastructure_project_rail_stations_v16;
             ALTER TABLE infrastructure_project_planned_stations RENAME TO infrastructure_project_planned_stations_v16;
             ALTER TABLE infrastructure_project_planned_lines RENAME TO infrastructure_project_planned_lines_v16;
             ALTER TABLE company RENAME TO company_v16;
             ALTER TABLE trains RENAME TO trains_v16;
             ALTER TABLE passenger_services RENAME TO passenger_services_v16;
             ALTER TABLE service_stops RENAME TO service_stops_v16;
             ALTER TABLE service_lines RENAME TO service_lines_v16;
             ALTER TABLE origin_destination_demand RENAME TO origin_destination_demand_v16;
             ALTER TABLE active_journeys RENAME TO active_journeys_v16;
             ALTER TABLE journey_passenger_groups RENAME TO journey_passenger_groups_v16;
             ALTER TABLE journey_receipts RENAME TO journey_receipts_v16;",
        )
        .map_err(|source| db_error("begin UUID migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(SCHEMA)
            .map_err(|source| db_error("create UUID schema in", path, source))?;

        connection
            .execute_batch(
                r#"
                INSERT INTO region(singleton, name, registration_code, registration_mark, population, rail_authority_name)
                SELECT singleton, name, registration_code, registration_mark, population, rail_authority_name
                FROM region_v16;

                INSERT INTO settlements(id, sequence, name, population)
                SELECT printf('00000001-0000-4000-8000-%012x', id), id, name, population
                FROM settlements_v16;

                INSERT INTO rail_stations(id, sequence, settlement_id)
                SELECT printf('00000002-0000-4000-8000-%012x', id), id,
                       printf('00000001-0000-4000-8000-%012x', settlement_id)
                FROM rail_stations_v16;

                INSERT INTO rail_lines(id, sequence, first_station_id, second_station_id, distance_metres,
                                       speed_limit_kmh, track_count, electrification, construction_difficulty)
                SELECT printf('00000003-0000-4000-8000-%012x', id), id,
                       printf('00000002-0000-4000-8000-%012x', first_station_id),
                       printf('00000002-0000-4000-8000-%012x', second_station_id),
                       distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                FROM rail_lines_v16;

                INSERT INTO infrastructure_projects(
                    id, sequence, kind, status, requested_at, review_started_at, proposed_at, approved_at,
                    funding_completed_at, scheduled_start_at, construction_started_at,
                    planned_completion_at, completed_at, deferred_at, cancelled_at,
                    target_speed_limit_kmh, target_track_count
                )
                SELECT printf('00000004-0000-4000-8000-%012x', id), id, kind, status, requested_at,
                       review_started_at, proposed_at, approved_at, funding_completed_at,
                       scheduled_start_at, construction_started_at, planned_completion_at,
                       completed_at, deferred_at, cancelled_at, target_speed_limit_kmh,
                       target_track_count
                FROM infrastructure_projects_v16;

                INSERT INTO infrastructure_project_rail_lines(project_id, sequence, rail_line_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', rail_line_id)
                FROM infrastructure_project_rail_lines_v16;

                INSERT INTO infrastructure_project_rail_stations(project_id, sequence, rail_station_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', rail_station_id)
                FROM infrastructure_project_rail_stations_v16;

                INSERT INTO infrastructure_project_planned_stations(project_id, sequence, station_id, settlement_id)
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', station_id),
                       printf('00000001-0000-4000-8000-%012x', settlement_id)
                FROM infrastructure_project_planned_stations_v16;

                INSERT INTO infrastructure_project_planned_lines(
                    project_id, sequence, line_id, first_station_id, second_station_id,
                    distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                )
                SELECT printf('00000004-0000-4000-8000-%012x', project_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', line_id),
                       printf('00000002-0000-4000-8000-%012x', first_station_id),
                       printf('00000002-0000-4000-8000-%012x', second_station_id),
                       distance_metres, speed_limit_kmh, track_count, electrification, construction_difficulty
                FROM infrastructure_project_planned_lines_v16;

                INSERT INTO company(singleton, name, vkm, funds_cents, next_train_display_number)
                SELECT singleton, name, vkm, funds_cents, next_train_id FROM company_v16;

                INSERT INTO trains(id, sequence, evn, nickname, status_kind, status_ref_id, model_id, original_purchase_price_cents)
                SELECT printf('00000005-0000-4000-8000-%012x', id), id, evn, nickname, status_kind,
                       CASE status_kind
                           WHEN 'ready' THEN printf('00000002-0000-4000-8000-%012x', status_ref_id)
                           ELSE printf('00000007-0000-4000-8000-%012x', status_ref_id)
                       END,
                       model_id, original_purchase_price_cents
                FROM trains_v16;

                INSERT INTO passenger_services(id, sequence, name)
                SELECT printf('00000006-0000-4000-8000-%012x', id), id, name
                FROM passenger_services_v16;

                INSERT INTO service_stops(service_id, sequence, station_id)
                SELECT printf('00000006-0000-4000-8000-%012x', service_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', station_id)
                FROM service_stops_v16;

                INSERT INTO service_lines(service_id, sequence, rail_line_id)
                SELECT printf('00000006-0000-4000-8000-%012x', service_id), sequence,
                       printf('00000003-0000-4000-8000-%012x', rail_line_id)
                FROM service_lines_v16;

                INSERT INTO origin_destination_demand(
                    origin_station_id, destination_station_id, sequence, waiting_passengers,
                    passenger_arrival_rate_per_hour, fractional_passenger_seconds
                )
                SELECT printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       ROW_NUMBER() OVER (ORDER BY origin_station_id, destination_station_id) - 1,
                       waiting_passengers, passenger_arrival_rate_per_hour, fractional_passenger_seconds
                FROM origin_destination_demand_v16;

                INSERT INTO active_journeys(
                    id, sequence, service_id, train_id, origin_station_id, destination_station_id,
                    passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents,
                    infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index,
                    departed_at, arrives_at
                )
                SELECT printf('00000007-0000-4000-8000-%012x', id), id,
                       printf('00000006-0000-4000-8000-%012x', service_id),
                       printf('00000005-0000-4000-8000-%012x', train_id),
                       printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       passengers_carried, fare_cents, operating_revenue_cents, credited_revenue_cents,
                       infrastructure_access_fee_cents, fuel_cost_cents, current_stop_index,
                       departed_at, arrives_at
                FROM active_journeys_v16;

                INSERT INTO journey_passenger_groups(
                    journey_id, sequence, origin_station_id, destination_station_id, passengers, fare_cents
                )
                SELECT printf('00000007-0000-4000-8000-%012x', journey_id), sequence,
                       printf('00000002-0000-4000-8000-%012x', origin_station_id),
                       printf('00000002-0000-4000-8000-%012x', destination_station_id),
                       passengers, fare_cents
                FROM journey_passenger_groups_v16;

                INSERT INTO journey_receipts(
                    journey_id, revenue_cents, infrastructure_access_fee_cents, fuel_cost_cents,
                    train_id, train_model_name, origin_station_id, destination_station_id,
                    passengers_carried, passenger_capacity, completed_at
                )
                SELECT printf('00000007-0000-4000-8000-%012x', journey_id), revenue_cents,
                       infrastructure_access_fee_cents, fuel_cost_cents,
                       CASE WHEN train_id IS NULL THEN NULL ELSE printf('00000005-0000-4000-8000-%012x', train_id) END,
                       train_model_name,
                       CASE WHEN origin_station_id IS NULL THEN NULL ELSE printf('00000002-0000-4000-8000-%012x', origin_station_id) END,
                       CASE WHEN destination_station_id IS NULL THEN NULL ELSE printf('00000002-0000-4000-8000-%012x', destination_station_id) END,
                       passengers_carried, passenger_capacity, completed_at
                FROM journey_receipts_v16;

                DROP TABLE journey_passenger_groups_v16;
                DROP TABLE active_journeys_v16;
                DROP TABLE origin_destination_demand_v16;
                DROP TABLE service_lines_v16;
                DROP TABLE service_stops_v16;
                DROP TABLE passenger_services_v16;
                DROP TABLE trains_v16;
                DROP TABLE company_v16;
                DROP TABLE infrastructure_project_planned_lines_v16;
                DROP TABLE infrastructure_project_planned_stations_v16;
                DROP TABLE infrastructure_project_rail_stations_v16;
                DROP TABLE infrastructure_project_rail_lines_v16;
                DROP TABLE infrastructure_projects_v16;
                DROP TABLE rail_lines_v16;
                DROP TABLE rail_stations_v16;
                DROP TABLE settlements_v16;
                DROP TABLE region_v16;
                DROP TABLE journey_receipts_v16;

                CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);
                CREATE INDEX IF NOT EXISTS idx_receipts_completed_at ON journey_receipts(completed_at);
                "#,
            )
            .map_err(|source| db_error("migrate entity IDs to UUID v4 in", path, source))?;

        connection
            .pragma_update(None, "user_version", 17_u32)
            .map_err(|source| db_error("write v17 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            connection
                .execute_batch(
                    "COMMIT;
                     PRAGMA legacy_alter_table = OFF;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|source| db_error("commit UUID migration for", path, source))?;
            let violation: Option<String> = connection
                .query_row(
                    "SELECT table FROM pragma_foreign_key_check LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| db_error("verify UUID migration for", path, source))?;
            if violation.is_some() {
                return Err(invalid_value(path, "foreign keys after UUID migration"));
            }
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch(
                "ROLLBACK;
                 PRAGMA legacy_alter_table = OFF;
                 PRAGMA foreign_keys = ON;",
            );
            Err(error)
        }
    }
}

fn migrate_v17_to_v18(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v17 to v18 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let has_capacity_column: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('region')
                 WHERE name = 'rail_authority_construction_capacity'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect Rail Authority construction capacity in",
                    path,
                    source,
                )
            })?;
        if has_capacity_column == 0 {
            connection
                .execute(
                    "ALTER TABLE region
                     ADD COLUMN rail_authority_construction_capacity INTEGER NOT NULL DEFAULT 1
                     CHECK (rail_authority_construction_capacity > 0)",
                    [],
                )
                .map_err(|source| {
                    db_error("add Rail Authority construction capacity to", path, source)
                })?;
        }
        connection
            .pragma_update(None, "user_version", 18_u32)
            .map_err(|source| db_error("write v18 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v17 to v18 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v18_to_v19(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v18 to v19 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let project_table_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'infrastructure_projects'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Infrastructure Project table in", path, source))?;

        if project_table_exists != 0 {
            let has_estimated_cost: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('infrastructure_projects')
                     WHERE name = 'estimated_cost_cents'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| db_error("inspect project estimated cost in", path, source))?;
            if has_estimated_cost == 0 {
                connection
                    .execute(
                        "ALTER TABLE infrastructure_projects
                         ADD COLUMN estimated_cost_cents INTEGER NOT NULL DEFAULT 0
                         CHECK (estimated_cost_cents >= 0)",
                        [],
                    )
                    .map_err(|source| db_error("add project estimated cost to", path, source))?;
            }

            let has_authority_commitment: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('infrastructure_projects')
                     WHERE name = 'authority_committed_cents'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    db_error("inspect project Authority commitment in", path, source)
                })?;
            if has_authority_commitment == 0 {
                connection
                    .execute(
                        "ALTER TABLE infrastructure_projects
                         ADD COLUMN authority_committed_cents INTEGER NOT NULL DEFAULT 0
                         CHECK (authority_committed_cents >= 0)",
                        [],
                    )
                    .map_err(|source| {
                        db_error("add project Authority commitment to", path, source)
                    })?;
            }

            let planned_lines_exist: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'infrastructure_project_planned_lines'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|source| db_error("inspect planned Rail Line table in", path, source))?;
            if planned_lines_exist != 0 {
                connection
                    .execute_batch(
                        "UPDATE infrastructure_projects
                         SET estimated_cost_cents = COALESCE((
                             SELECT SUM(
                                 ((pl.distance_metres * CASE pl.construction_difficulty
                                     WHEN 'low' THEN 120000
                                     WHEN 'moderate' THEN 160000
                                     WHEN 'high' THEN 220000
                                     ELSE 160000
                                 END) + 999) / 1000
                             )
                             FROM infrastructure_project_planned_lines pl
                             WHERE pl.project_id = infrastructure_projects.id
                         ), 0)
                         WHERE kind = 'new_line' AND estimated_cost_cents = 0;",
                    )
                    .map_err(|source| {
                        db_error("backfill project estimated cost in", path, source)
                    })?;
            }
        }

        connection
            .pragma_update(None, "user_version", 19_u32)
            .map_err(|source| db_error("write v19 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v18 to v19 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v19_to_v20(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v19 to v20 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE settlements ADD COLUMN world_x INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE settlements ADD COLUMN world_y INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|source| db_error("add Settlement coordinates to", path, source))?;

        let world_seed_text: String = connection
            .query_row(
                "SELECT world_seed FROM game_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "read world seed for Settlement coordinate migration from",
                    path,
                    source,
                )
            })?;
        let world_seed =
            world_seed_text
                .parse::<u64>()
                .map_err(|_| SaveSlotError::InvalidSave {
                    path: path.to_path_buf(),
                    source: Box::new(SaveCodecError::InvalidValue {
                        field: "World Seed",
                    }),
                })?;
        let settlement_count: usize = connection
            .query_row("SELECT COUNT(*) FROM settlements", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| {
                db_error(
                    "count Settlements for coordinate migration in",
                    path,
                    source,
                )
            })?
            .try_into()
            .map_err(|_| SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "Settlement Count",
                }),
            })?;
        let positions = settlement_positions_for_existing_region(world_seed, settlement_count);
        for (sequence, position) in positions.into_iter().enumerate() {
            connection
                .execute(
                    "UPDATE settlements SET world_x = ?1, world_y = ?2 WHERE sequence = ?3",
                    params![
                        position.x,
                        position.y,
                        i64::try_from(sequence).unwrap_or(i64::MAX)
                    ],
                )
                .map_err(|source| db_error("backfill Settlement coordinates in", path, source))?;
        }

        connection
            .pragma_update(None, "user_version", 20_u32)
            .map_err(|source| db_error("write v20 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v19 to v20 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v20_to_v21(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v20 to v21 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE infrastructure_projects
                 ADD COLUMN operator_contributed_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (operator_contributed_cents >= 0);",
            )
            .map_err(|source| {
                db_error("add operator infrastructure contributions to", path, source)
            })?;
        connection
            .pragma_update(None, "user_version", 21_u32)
            .map_err(|source| db_error("write v21 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v20 to v21 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v21_to_v22(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v21 to v22 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE infrastructure_projects
                 ADD COLUMN access_fee_credit_awarded_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (access_fee_credit_awarded_cents >= 0);
                 ALTER TABLE infrastructure_projects
                 ADD COLUMN access_fee_credit_remaining_cents INTEGER NOT NULL DEFAULT 0
                 CHECK (access_fee_credit_remaining_cents >= 0);
                 UPDATE infrastructure_projects
                 SET access_fee_credit_awarded_cents = (operator_contributed_cents * 115) / 100,
                     access_fee_credit_remaining_cents = (operator_contributed_cents * 115) / 100
                 WHERE status = 'open' AND operator_contributed_cents > 0;",
            )
            .map_err(|source| db_error("add infrastructure access credits to", path, source))?;
        connection
            .pragma_update(None, "user_version", 22_u32)
            .map_err(|source| db_error("write v22 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v21 to v22 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v22_to_v23(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v22 to v23 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let finances_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_authority_finances'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Authority finances during v23 migration in", path, source))?;
        if finances_exist != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE rail_authority_finances
                     ADD COLUMN next_fiscal_period_at INTEGER;",
                )
                .map_err(|source| {
                    db_error("add Rail Authority fiscal calendar to", path, source)
                })?;
        }

        let game_meta_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'game_meta'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect game metadata during v23 migration in",
                    path,
                    source,
                )
            })?;
        if finances_exist != 0 && game_meta_exists != 0 {
            connection
                .execute_batch(
                    "UPDATE rail_authority_finances
                     SET next_fiscal_period_at = (
                         SELECT last_processed_at + 21600
                         FROM game_meta
                         WHERE singleton = 1
                     )
                     WHERE singleton = 1;",
                )
                .map_err(|source| {
                    db_error("seed Rail Authority fiscal calendar in", path, source)
                })?;
        }

        connection
            .pragma_update(None, "user_version", 23_u32)
            .map_err(|source| db_error("write v23 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v22 to v23 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v23_to_v24(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v23 to v24 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let finances_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rail_authority_finances'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| db_error("inspect Authority finances during v24 migration in", path, source))?;
        let game_meta_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'game_meta'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect game metadata during v24 migration in",
                    path,
                    source,
                )
            })?;

        if finances_exist != 0 && game_meta_exists != 0 {
            connection
                .execute_batch(
                    "UPDATE rail_authority_finances
                     SET next_fiscal_period_at = (
                         SELECT ((last_processed_at / 86400) + 1) * 86400
                         FROM game_meta
                         WHERE singleton = 1
                     )
                     WHERE singleton = 1;",
                )
                .map_err(|source| {
                    db_error(
                        "align Authority fiscal calendar to UTC midnight in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 24_u32)
            .map_err(|source| db_error("write v24 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v23 to v24 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v24_to_v25(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v24 to v25 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let demand_table_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'origin_destination_demand'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect Passenger Demand during v25 migration in",
                    path,
                    source,
                )
            })?;

        if demand_table_exists != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE origin_destination_demand
                     ADD COLUMN market_maturity_basis_points INTEGER NOT NULL DEFAULT 10000
                     CHECK (market_maturity_basis_points BETWEEN 0 AND 10000);",
                )
                .map_err(|source| {
                    db_error(
                        "add Passenger Demand maturity during v25 migration in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 25_u32)
            .map_err(|source| db_error("write v25 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v24 to v25 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v25_to_v26(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v25 to v26 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        let projects_exist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'infrastructure_projects'",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                db_error(
                    "inspect infrastructure projects during v26 migration in",
                    path,
                    source,
                )
            })?;

        if projects_exist != 0 {
            connection
                .execute_batch(
                    "ALTER TABLE infrastructure_projects
                     ADD COLUMN reconsideration_count INTEGER NOT NULL DEFAULT 0
                     CHECK (reconsideration_count >= 0);",
                )
                .map_err(|source| {
                    db_error(
                        "add project reconsideration count during v26 migration in",
                        path,
                        source,
                    )
                })?;
        }

        connection
            .pragma_update(None, "user_version", 26_u32)
            .map_err(|source| db_error("write v26 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v25 to v26 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v26_to_v27(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v26 to v27 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS bulletin_entries (
                     sequence INTEGER PRIMARY KEY,
                     occurred_at INTEGER NOT NULL,
                     category TEXT NOT NULL CHECK (category IN ('local', 'authority', 'construction', 'network')),
                     headline TEXT NOT NULL,
                     detail TEXT NOT NULL
                 );",
            )
            .map_err(|source| db_error("add Railway Bulletin history to", path, source))?;
        connection
            .pragma_update(None, "user_version", 27_u32)
            .map_err(|source| db_error("write v27 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v26 to v27 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v27_to_v28(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v27 to v28 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE passenger_services
                 ADD COLUMN direction_mode TEXT NOT NULL DEFAULT 'both'
                 CHECK (direction_mode IN ('both', 'forward'));",
            )
            .map_err(|source| {
                db_error(
                    "add Passenger Service direction mode during v28 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .pragma_update(None, "user_version", 28_u32)
            .map_err(|source| db_error("write v28 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v27 to v28 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

#[derive(Clone, Debug)]
struct V28PassengerServiceRoute {
    id: String,
    sequence: i64,
    stop_station_ids: Vec<String>,
    rail_line_ids: Vec<String>,
    has_active_journey: bool,
    reversible: bool,
}

fn migrate_v28_to_v29(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v28 to v29 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute(
                "UPDATE passenger_services SET direction_mode = 'forward'",
                [],
            )
            .map_err(|source| {
                db_error(
                    "reset Passenger Service direction modes during v29 migration in",
                    path,
                    source,
                )
            })?;

        let required_tables = ["service_stops", "service_lines", "rail_lines"];
        let can_inspect_routes = required_tables
            .iter()
            .map(|table| migration_table_exists(connection, path, table))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|exists| exists);

        if can_inspect_routes {
            migrate_v28_passenger_services(connection, path)?;
        }

        connection
            .pragma_update(None, "user_version", 29_u32)
            .map_err(|source| db_error("write v29 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v28 to v29 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v28_passenger_services(
    connection: &Connection,
    path: &Path,
) -> Result<(), SaveSlotError> {
    let line_endpoints = query_all(
        connection,
        "SELECT id, first_station_id, second_station_id FROM rail_lines",
        path,
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?
    .into_iter()
    .map(|(line_id, first_station_id, second_station_id)| {
        (line_id, (first_station_id, second_station_id))
    })
    .collect::<HashMap<_, _>>();

    let active_service_ids = if migration_table_exists(connection, path, "active_journeys")? {
        query_all(
            connection,
            "SELECT DISTINCT service_id FROM active_journeys",
            path,
            |row| row.get::<_, String>(0),
        )?
        .into_iter()
        .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };

    let service_rows = query_all(
        connection,
        "SELECT id, sequence FROM passenger_services ORDER BY sequence",
        path,
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;

    let mut services = Vec::with_capacity(service_rows.len());
    for (service_id, sequence) in service_rows {
        let stop_station_ids = ordered_service_ids(
            connection,
            path,
            "SELECT station_id FROM service_stops WHERE service_id = ?1 ORDER BY sequence",
            &service_id,
        )?;
        let rail_line_ids = ordered_service_ids(
            connection,
            path,
            "SELECT rail_line_id FROM service_lines WHERE service_id = ?1 ORDER BY sequence",
            &service_id,
        )?;
        let reversible =
            stored_service_route_is_reversible(&stop_station_ids, &rail_line_ids, &line_endpoints);
        services.push(V28PassengerServiceRoute {
            has_active_journey: active_service_ids.contains(&service_id),
            id: service_id,
            sequence,
            stop_station_ids,
            rail_line_ids,
            reversible,
        });
    }

    let mut handled = HashSet::<String>::new();
    for index in 0..services.len() {
        let service = &services[index];
        if handled.contains(&service.id) {
            continue;
        }
        handled.insert(service.id.clone());

        if !service.reversible {
            continue;
        }

        let reciprocal_index = services
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, candidate)| {
                !handled.contains(&candidate.id)
                    && candidate.reversible
                    && routes_are_exact_reciprocals(service, candidate)
            })
            .map(|(candidate_index, _)| candidate_index);

        let Some(reciprocal_index) = reciprocal_index else {
            set_service_direction_mode(connection, path, &service.id, "both")?;
            continue;
        };

        let reciprocal = &services[reciprocal_index];
        handled.insert(reciprocal.id.clone());

        if service.has_active_journey && reciprocal.has_active_journey {
            continue;
        }

        let (survivor, retired) = if service.has_active_journey {
            (service, reciprocal)
        } else if reciprocal.has_active_journey {
            (reciprocal, service)
        } else if service.sequence <= reciprocal.sequence {
            (service, reciprocal)
        } else {
            (reciprocal, service)
        };

        set_service_direction_mode(connection, path, &survivor.id, "both")?;
        connection
            .execute(
                "DELETE FROM service_stops WHERE service_id = ?1",
                params![&retired.id],
            )
            .map_err(|source| {
                db_error(
                    "remove reciprocal Passenger Service stops during v29 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .execute(
                "DELETE FROM service_lines WHERE service_id = ?1",
                params![&retired.id],
            )
            .map_err(|source| {
                db_error(
                    "remove reciprocal Passenger Service lines during v29 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .execute(
                "DELETE FROM passenger_services WHERE id = ?1",
                params![&retired.id],
            )
            .map_err(|source| {
                db_error(
                    "merge reciprocal Passenger Services during v29 migration in",
                    path,
                    source,
                )
            })?;
    }

    Ok(())
}

fn migration_table_exists(
    connection: &Connection,
    path: &Path,
    table_name: &str,
) -> Result<bool, SaveSlotError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table_name],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(|source| db_error("inspect migration schema in", path, source))
}

fn ordered_service_ids(
    connection: &Connection,
    path: &Path,
    sql: &str,
    service_id: &str,
) -> Result<Vec<String>, SaveSlotError> {
    let mut statement = connection.prepare(sql).map_err(|source| {
        db_error(
            "prepare Passenger Service migration query for",
            path,
            source,
        )
    })?;
    let rows = statement
        .query_map(params![service_id], |row| row.get::<_, String>(0))
        .map_err(|source| db_error("query Passenger Service migration data from", path, source))?;
    rows.map(|row| {
        row.map_err(|source| db_error("decode Passenger Service migration row from", path, source))
    })
    .collect()
}

fn stored_service_route_is_reversible(
    stop_station_ids: &[String],
    rail_line_ids: &[String],
    line_endpoints: &HashMap<String, (String, String)>,
) -> bool {
    if stop_station_ids.len() < 2 || rail_line_ids.is_empty() {
        return false;
    }
    if stop_station_ids.iter().collect::<HashSet<_>>().len() != stop_station_ids.len()
        || rail_line_ids.iter().collect::<HashSet<_>>().len() != rail_line_ids.len()
    {
        return false;
    }

    let mut current_station_id = stop_station_ids[0].as_str();
    let mut traversed_station_ids = vec![current_station_id];
    for rail_line_id in rail_line_ids {
        let Some((first_station_id, second_station_id)) = line_endpoints.get(rail_line_id) else {
            return false;
        };
        current_station_id = if current_station_id == first_station_id.as_str() {
            second_station_id.as_str()
        } else if current_station_id == second_station_id.as_str() {
            first_station_id.as_str()
        } else {
            return false;
        };
        traversed_station_ids.push(current_station_id);
    }

    if current_station_id
        != stop_station_ids
            .last()
            .expect("at least two stops")
            .as_str()
    {
        return false;
    }

    let mut path_index = 0_usize;
    for stop_station_id in stop_station_ids.iter().skip(1) {
        let Some(relative_index) = traversed_station_ids[path_index + 1..]
            .iter()
            .position(|station_id| *station_id == stop_station_id.as_str())
        else {
            return false;
        };
        path_index += relative_index + 1;
    }

    path_index == traversed_station_ids.len() - 1
}

fn routes_are_exact_reciprocals(
    first: &V28PassengerServiceRoute,
    second: &V28PassengerServiceRoute,
) -> bool {
    first.stop_station_ids.len() == second.stop_station_ids.len()
        && first.rail_line_ids.len() == second.rail_line_ids.len()
        && first
            .stop_station_ids
            .iter()
            .eq(second.stop_station_ids.iter().rev())
        && first
            .rail_line_ids
            .iter()
            .eq(second.rail_line_ids.iter().rev())
}

fn set_service_direction_mode(
    connection: &Connection,
    path: &Path,
    service_id: &str,
    direction_mode: &str,
) -> Result<(), SaveSlotError> {
    connection
        .execute(
            "UPDATE passenger_services SET direction_mode = ?1 WHERE id = ?2",
            params![direction_mode, service_id],
        )
        .map_err(|source| {
            db_error(
                "set Passenger Service direction mode during v29 migration in",
                path,
                source,
            )
        })?;
    Ok(())
}

fn migrate_v29_to_v30(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v29 to v30 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE passenger_services ADD COLUMN forward_train_number INTEGER;
                 ALTER TABLE passenger_services ADD COLUMN reverse_train_number INTEGER;",
            )
            .map_err(|source| {
                db_error(
                    "add Passenger Service train numbers during v30 migration in",
                    path,
                    source,
                )
            })?;

        let services = query_all(
            connection,
            "SELECT id, direction_mode FROM passenger_services ORDER BY sequence",
            path,
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut next_train_number = 100_i64;
        for (service_id, direction_mode) in services {
            let reverse_train_number = if direction_mode == "both" {
                let reverse =
                    next_train_number
                        .checked_add(1)
                        .ok_or_else(|| SaveSlotError::InvalidSave {
                            path: path.to_path_buf(),
                            source: Box::new(SaveCodecError::InvalidValue {
                                field: "Passenger Service train number",
                            }),
                        })?;
                Some(reverse)
            } else {
                None
            };
            connection
                .execute(
                    "UPDATE passenger_services
                     SET forward_train_number = ?1, reverse_train_number = ?2
                     WHERE id = ?3",
                    params![next_train_number, reverse_train_number, service_id],
                )
                .map_err(|source| {
                    db_error(
                        "assign Passenger Service train numbers during v30 migration in",
                        path,
                        source,
                    )
                })?;
            next_train_number = match reverse_train_number {
                Some(reverse) => reverse.checked_add(1),
                None => next_train_number.checked_add(1),
            }
            .ok_or_else(|| SaveSlotError::InvalidSave {
                path: path.to_path_buf(),
                source: Box::new(SaveCodecError::InvalidValue {
                    field: "Passenger Service train number",
                }),
            })?;
        }

        connection
            .execute_batch(
                "CREATE UNIQUE INDEX passenger_services_forward_train_number_idx
                     ON passenger_services(forward_train_number);
                 CREATE UNIQUE INDEX passenger_services_reverse_train_number_idx
                     ON passenger_services(reverse_train_number)
                     WHERE reverse_train_number IS NOT NULL;",
            )
            .map_err(|source| {
                db_error(
                    "index Passenger Service train numbers during v30 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .pragma_update(None, "user_version", 30_u32)
            .map_err(|source| db_error("write v30 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v29 to v30 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v30_to_v31(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v30 to v31 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "CREATE TABLE train_service_assignments (
                     train_id TEXT PRIMARY KEY REFERENCES trains(id) ON DELETE CASCADE,
                     service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE
                 );
                 CREATE INDEX train_service_assignments_service_idx
                     ON train_service_assignments(service_id);",
            )
            .map_err(|source| {
                db_error(
                    "create Train Passenger Service assignments during v31 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .pragma_update(None, "user_version", 31_u32)
            .map_err(|source| db_error("write v31 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v30 to v31 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v31_to_v32(connection: &Connection, path: &Path) -> Result<(), SaveSlotError> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(|source| db_error("begin v31 to v32 migration for", path, source))?;

    let migration = (|| -> Result<(), SaveSlotError> {
        connection
            .execute_batch(
                "ALTER TABLE passenger_services ADD COLUMN custom_name TEXT
                     CHECK (custom_name IS NULL OR length(trim(custom_name)) BETWEEN 1 AND 32);",
            )
            .map_err(|source| {
                db_error(
                    "add Passenger Service commercial names during v32 migration in",
                    path,
                    source,
                )
            })?;
        connection
            .pragma_update(None, "user_version", 32_u32)
            .map_err(|source| db_error("write v32 schema version to", path, source))?;
        Ok(())
    })();

    match migration {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(|source| db_error("commit v31 to v32 migration for", path, source)),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

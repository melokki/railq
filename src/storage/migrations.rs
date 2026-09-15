use std::path::Path;

use rusqlite::Connection;

use super::{
    SaveCodecError, SaveSlotError, SAVE_VERSION, db_error, migrate_v1_to_v2, migrate_v2_to_v3,
    migrate_v3_to_v4, migrate_v4_to_v5, migrate_v5_to_v6, migrate_v6_to_v7, migrate_v7_to_v8,
    migrate_v8_to_v9, migrate_v9_to_v10, migrate_v10_to_v11, migrate_v11_to_v12,
    migrate_v12_to_v13, migrate_v13_to_v14, migrate_v14_to_v15, migrate_v15_to_v16,
    migrate_v16_to_v17, migrate_v17_to_v18, migrate_v18_to_v19, migrate_v19_to_v20,
    migrate_v20_to_v21, migrate_v21_to_v22, migrate_v22_to_v23, migrate_v23_to_v24,
    migrate_v24_to_v25, migrate_v25_to_v26, migrate_v26_to_v27,
    schema::SCHEMA,
};

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

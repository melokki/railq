//! Save-slot ownership and lifecycle.
//!
//! This module owns the filesystem lock, SQLite connection lifecycle, backup
//! handling, and the load/save transaction boundary. The rest of the storage
//! package stays focused on schema, migrations, state mapping, and validation.

use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs4::{FileExt, TryLockError};
use rusqlite::{Connection, OptionalExtension};

use crate::model::GameState;

use super::{
    DEFAULT_SAVE_PATH, LEGACY_SAVE_PATH,
    legacy::decode_legacy_game_state,
    migrations::ensure_schema,
    state_io::{clear_state, insert_state, load_state},
    validation::{SaveValidationError, validate_game_state},
};

static NEXT_ARCHIVE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct SaveSlot {
    path: PathBuf,
    _lock_file: File,
}

#[derive(Debug)]
pub enum SaveSlotError {
    InvalidPath { path: PathBuf },
    AlreadyOwned { path: PathBuf },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Database {
        action: &'static str,
        path: PathBuf,
        source: rusqlite::Error,
    },
    InvalidSave {
        path: PathBuf,
        source: Box<SaveCodecError>,
    },
}

impl fmt::Display for SaveSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => {
                write!(formatter, "save path {} has no file name", path.display())
            }
            Self::AlreadyOwned { path } => {
                write!(formatter, "save slot {} is already owned", path.display())
            }
            Self::Io { action, path, source } => {
                write!(formatter, "could not {action} {}: {source}", path.display())
            }
            Self::Database { action, path, source } => write!(
                formatter,
                "could not {action} SQLite save {}: {source}",
                path.display()
            ),
            Self::InvalidSave { path, source } => write!(
                formatter,
                "save {} is invalid and was preserved: {source}",
                path.display()
            ),
        }
    }
}

impl Error for SaveSlotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Database { source, .. } => Some(source),
            Self::InvalidSave { source, .. } => Some(source),
            Self::InvalidPath { .. } | Self::AlreadyOwned { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveCodecError {
    UnsupportedVersion { found: u32 },
    InvalidState(SaveValidationError),
    InvalidValue { field: &'static str },
    TrainModelNotFound { model_name: String },
    LegacyDecode(String),
}

impl fmt::Display for SaveCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => {
                write!(formatter, "save version {found} is not supported")
            }
            Self::InvalidState(error) => error.fmt(formatter),
            Self::InvalidValue { field } => write!(formatter, "invalid SQLite value for {field}"),
            Self::TrainModelNotFound { model_name } => write!(
                formatter,
                "saved Train model {model_name:?} does not exist in the central catalogue"
            ),
            Self::LegacyDecode(error) => {
                write!(formatter, "could not decode legacy RON save: {error}")
            }
        }
    }
}

impl Error for SaveCodecError {}

impl SaveSlot {
    pub fn open_default() -> Result<Self, SaveSlotError> {
        let database_existed = Path::new(DEFAULT_SAVE_PATH).exists();
        let slot = Self::open(DEFAULT_SAVE_PATH)?;
        if !database_existed && Path::new(LEGACY_SAVE_PATH).exists() {
            let source = fs::read_to_string(LEGACY_SAVE_PATH).map_err(|source| SaveSlotError::Io {
                action: "read legacy RON save",
                path: PathBuf::from(LEGACY_SAVE_PATH),
                source,
            })?;
            let state = decode_legacy_game_state(&source).map_err(|source| SaveSlotError::InvalidSave {
                path: PathBuf::from(LEGACY_SAVE_PATH),
                source: Box::new(source),
            })?;
            slot.save(&state)?;
        }
        Ok(slot)
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SaveSlotError> {
        let path = path.into();
        let lock_path = sidecar_lock_path(&path)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| SaveSlotError::Io {
                action: "open save lock",
                path: lock_path.clone(),
                source,
            })?;
        match FileExt::try_lock(&lock_file) {
            Ok(()) => Ok(Self { path, _lock_file: lock_file }),
            Err(TryLockError::WouldBlock) => Err(SaveSlotError::AlreadyOwned { path }),
            Err(TryLockError::Error(source)) => Err(SaveSlotError::Io {
                action: "lock save slot",
                path,
                source,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<GameState>, SaveSlotError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let connection = self.open_connection("open")?;
        ensure_schema(&connection, &self.path)?;
        let state = load_state(&connection, &self.path)?;
        if let Some(state) = &state {
            validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
                path: self.path.clone(),
                source: Box::new(SaveCodecError::InvalidState(source)),
            })?;
        }
        Ok(state)
    }

    pub fn save(&self, state: &GameState) -> Result<(), SaveSlotError> {
        validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(SaveCodecError::InvalidState(source)),
        })?;
        if self.path.exists() {
            self.load()?;
        }
        self.replace_state(state)
    }

    pub fn save_after_backup(&self, state: &GameState) -> Result<PathBuf, SaveSlotError> {
        self.load()?;
        validate_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(SaveCodecError::InvalidState(source)),
        })?;
        let backup_path = archive_save(&self.path).map_err(|source| SaveSlotError::Io {
            action: "archive existing save before restart",
            path: self.path.clone(),
            source,
        })?;
        self.replace_state(state)?;
        Ok(backup_path)
    }

    fn replace_state(&self, state: &GameState) -> Result<(), SaveSlotError> {
        let mut connection = self.open_connection("open for write")?;
        ensure_schema(&connection, &self.path)?;
        let existing_world_seed = connection
            .query_row(
                "SELECT world_seed FROM game_meta WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| SaveSlotError::Database {
                action: "read existing game identity from",
                path: self.path.clone(),
                source,
            })?;
        let current_world_seed = state.world_seed.to_string();
        let preserve_history = existing_world_seed.as_deref() == Some(current_world_seed.as_str());
        let transaction = connection.transaction().map_err(|source| SaveSlotError::Database {
            action: "begin transaction for",
            path: self.path.clone(),
            source,
        })?;
        clear_state(&transaction, &self.path, preserve_history)?;
        insert_state(&transaction, state, &self.path)?;
        transaction.commit().map_err(|source| SaveSlotError::Database {
            action: "commit transaction for",
            path: self.path.clone(),
            source,
        })?;
        connection.execute_batch("PRAGMA optimize;").map_err(|source| SaveSlotError::Database {
            action: "optimize",
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    fn open_connection(&self, action: &'static str) -> Result<Connection, SaveSlotError> {
        let connection = Connection::open(&self.path).map_err(|source| SaveSlotError::Database {
            action,
            path: self.path.clone(),
            source,
        })?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
            .map_err(|source| SaveSlotError::Database {
                action: "configure",
                path: self.path.clone(),
                source,
            })?;
        Ok(connection)
    }
}

fn sidecar_lock_path(path: &Path) -> Result<PathBuf, SaveSlotError> {
    let Some(file_name) = path.file_name() else {
        return Err(SaveSlotError::InvalidPath { path: path.to_path_buf() });
    };
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

fn archive_save(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "save path has no file name"))?;
    for _ in 0..128 {
        let sequence = NEXT_ARCHIVE_ID.fetch_add(1, Ordering::Relaxed);
        let archive_path = parent.join(format!(
            "{}.bankrupt-backup-{}.{}.db",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&archive_path) {
            Ok(_) => {
                fs::copy(path, &archive_path)?;
                return Ok(archive_path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique restart backup save",
    ))
}

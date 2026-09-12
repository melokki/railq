//! Versioned, validated RON representation of a RailQ game.
//!
//! This module owns the versioned serialization boundary and one local save
//! slot. The slot keeps an exclusive sidecar lock for its lifetime, so a
//! malformed save can be reported without being overwritten by another game.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    hash::Hash,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};

use crate::{
    model::{
        CalculationError, DistanceMetres, GameState, Journey, PassengerService, RailLine,
        RailLineId, RailNetwork, RailStationId, ServiceId, Train, TrainId, TrainStatus,
    },
    sim::economy::quote_journey,
};

/// The only save format understood by this build.
pub const SAVE_VERSION: u32 = 1;

/// The local save filename used when no explicit save path is supplied.
pub const DEFAULT_SAVE_PATH: &str = "railq.ron";

static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A path-bound, exclusively owned local save slot.
///
/// The sidecar lock remains open for the lifetime of this value. Its lock is
/// released automatically when the slot is dropped. `open` accepts an
/// explicit path for command-line overrides and temporary test scenarios.
#[derive(Debug)]
pub struct SaveSlot {
    path: PathBuf,
    _lock_file: File,
}

/// Why a local save slot cannot be opened, read, or replaced safely.
#[derive(Debug)]
pub enum SaveSlotError {
    /// The requested path cannot name a save file and its sidecar lock.
    InvalidPath { path: PathBuf },
    /// Another process already owns this save slot.
    AlreadyOwned { path: PathBuf },
    /// Disk I/O failed before the replacement could safely complete.
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The existing save is corrupt, unsupported, or invalid and was left in place.
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
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "could not {action} {}: {source}", path.display()),
            Self::InvalidSave { path, source } => {
                write!(
                    formatter,
                    "save {} is invalid and was preserved: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for SaveSlotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidSave { source, .. } => Some(source),
            Self::InvalidPath { .. } | Self::AlreadyOwned { .. } => None,
        }
    }
}

impl SaveSlot {
    /// Opens the default local save slot in the current directory.
    pub fn open_default() -> Result<Self, SaveSlotError> {
        Self::open(DEFAULT_SAVE_PATH)
    }

    /// Opens and exclusively owns the save slot at `path`.
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
            Ok(()) => Ok(Self {
                path,
                _lock_file: lock_file,
            }),
            Err(TryLockError::WouldBlock) => Err(SaveSlotError::AlreadyOwned { path }),
            Err(TryLockError::Error(source)) => Err(SaveSlotError::Io {
                action: "lock save slot",
                path,
                source,
            }),
        }
    }

    /// Returns the explicit path this slot owns.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a validated game, or reports that no save exists yet.
    ///
    /// An invalid existing save is never interpreted as a fresh Player
    /// Company; callers receive `InvalidSave` and the source remains intact.
    pub fn load(&self) -> Result<Option<GameState>, SaveSlotError> {
        let source = match fs::read(&self.path) {
            Ok(source) => source,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SaveSlotError::Io {
                    action: "read save",
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let source = String::from_utf8(source).map_err(|_| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(SaveCodecError::InvalidTextEncoding),
        })?;
        decode_game_state(&source)
            .map(Some)
            .map_err(|source| SaveSlotError::InvalidSave {
                path: self.path.clone(),
                source: Box::new(source),
            })
    }

    /// Validates and atomically replaces this slot's save.
    ///
    /// Before replacing anything, an existing save is decoded and validated.
    /// This refuses to overwrite corrupt or unsupported data, even if the
    /// caller is trying to create a new Player Company.
    pub fn save(&self, state: &GameState) -> Result<(), SaveSlotError> {
        self.save_with_before_replace(state, |_| Ok(()))
    }

    /// Preserves the current valid save under a unique sibling name before
    /// atomically replacing the slot with a fresh Player Company.
    ///
    /// The archive is created before the live save is replaced. If writing the
    /// replacement fails, both the live save and its preserved archive remain
    /// available to the player.
    pub fn save_after_backup(&self, state: &GameState) -> Result<PathBuf, SaveSlotError> {
        self.load()?;
        let encoded = encode_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(source),
        })?;
        let backup_path = archive_save(&self.path).map_err(|source| SaveSlotError::Io {
            action: "archive existing save before restart",
            path: self.path.clone(),
            source,
        })?;
        write_save_atomically(&self.path, encoded.as_bytes(), |_| Ok(())).map_err(|source| {
            SaveSlotError::Io {
                action: "replace save atomically after restart backup",
                path: self.path.clone(),
                source,
            }
        })?;
        Ok(backup_path)
    }

    fn save_with_before_replace<F>(
        &self,
        state: &GameState,
        before_replace: F,
    ) -> Result<(), SaveSlotError>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        self.load()?;
        let encoded = encode_game_state(state).map_err(|source| SaveSlotError::InvalidSave {
            path: self.path.clone(),
            source: Box::new(source),
        })?;
        write_save_atomically(&self.path, encoded.as_bytes(), before_replace).map_err(|source| {
            SaveSlotError::Io {
                action: "replace save atomically",
                path: self.path.clone(),
                source,
            }
        })
    }
}

fn sidecar_lock_path(path: &Path) -> Result<PathBuf, SaveSlotError> {
    let Some(file_name) = path.file_name() else {
        return Err(SaveSlotError::InvalidPath {
            path: path.to_path_buf(),
        });
    };
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

/// Copies an existing save to a unique, visible sibling archive without ever
/// selecting an already-existing archive name.
fn archive_save(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "save path has no file name"))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let archive_path = parent.join(format!(
            "{}.bankrupt-backup-{}.{}.ron",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        let mut archive = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive_path)
        {
            Ok(archive) => archive,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let copy_result = (|| {
            let mut source = File::open(path)?;
            io::copy(&mut source, &mut archive)?;
            archive.sync_all()
        })();
        match copy_result {
            Ok(()) => return Ok(archive_path),
            Err(error) => {
                drop(archive);
                let _ = fs::remove_file(&archive_path);
                return Err(error);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique restart backup save",
    ))
}

fn write_save_atomically<F>(path: &Path, encoded: &[u8], before_replace: F) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let mut temporary_save = TemporarySave::create_beside(path)?;
    temporary_save.file_mut().write_all(encoded)?;
    temporary_save.file_mut().sync_all()?;
    before_replace(temporary_save.path())?;
    temporary_save.replace(path)
}

/// A same-directory temporary save that removes itself if replacement fails.
struct TemporarySave {
    path: PathBuf,
    file: Option<File>,
}

impl TemporarySave {
    fn create_beside(save_path: &Path) -> io::Result<Self> {
        let parent = save_path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = save_path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "save path has no file name")
        })?;
        for _ in 0..128 {
            let sequence = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary_path = parent.join(format!(
                ".{}.{}.{}.tmp",
                file_name.to_string_lossy(),
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path: temporary_path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique same-directory temporary save",
        ))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temporary save file exists before replacement")
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn replace(mut self, save_path: &Path) -> io::Result<()> {
        drop(self.file.take());
        fs::rename(&self.path, save_path)
    }
}

impl Drop for TemporarySave {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

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

/// Why a save cannot be encoded or decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveCodecError {
    /// RON could not represent a valid save.
    Encode(ron::Error),
    /// The source is not valid UTF-8 text and therefore cannot be RON.
    InvalidTextEncoding,
    /// The source was not valid RON for the save envelope.
    Decode(ron::error::SpannedError),
    /// The save was written by a format this build does not understand.
    UnsupportedVersion { found: u32 },
    /// The RON decoded but cannot safely enter the simulation.
    InvalidState(SaveValidationError),
}

impl fmt::Display for SaveCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "could not encode save as RON: {error}"),
            Self::InvalidTextEncoding => write!(formatter, "save is not valid UTF-8 RON text"),
            Self::Decode(error) => write!(formatter, "could not decode save RON: {error}"),
            Self::UnsupportedVersion { found } => {
                write!(formatter, "save version {found} is not supported")
            }
            Self::InvalidState(error) => error.fmt(formatter),
        }
    }
}

impl Error for SaveCodecError {}

/// Encodes a valid game state in the current versioned RON envelope.
pub fn encode_game_state(state: &GameState) -> Result<String, SaveCodecError> {
    validate_game_state(state).map_err(SaveCodecError::InvalidState)?;
    ron::ser::to_string_pretty(
        &SaveEnvelope {
            version: SAVE_VERSION,
            state,
        },
        ron::ser::PrettyConfig::new(),
    )
    .map_err(SaveCodecError::Encode)
}

/// Decodes RON only after checking its version and every simulation invariant.
pub fn decode_game_state(source: &str) -> Result<GameState, SaveCodecError> {
    let envelope: SaveEnvelope = ron::from_str(source).map_err(SaveCodecError::Decode)?;
    if envelope.version != SAVE_VERSION {
        return Err(SaveCodecError::UnsupportedVersion {
            found: envelope.version,
        });
    }
    validate_game_state(&envelope.state).map_err(SaveCodecError::InvalidState)?;
    Ok(envelope.state)
}

/// Validates a state before it is saved or admitted from a decoded save.
pub fn validate_game_state(state: &GameState) -> Result<(), SaveValidationError> {
    validate_rules(state)?;

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
        .any(|settlement| settlement.id.get() == 0)
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
        .any(|station| station.id.get() == 0)
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
    if network.rail_lines.iter().any(|line| line.id.get() == 0) {
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
        .any(|service| service.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId {
            kind: "Passenger Service",
        });
    }
    for service in &state.player_company.passenger_services {
        if !station_ids.contains(&service.first_station_id)
            || !station_ids.contains(&service.second_station_id)
        {
            return Err(SaveValidationError::DanglingReference {
                field: "Passenger Service endpoint",
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
        .any(|train| train.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId { kind: "Train" });
    }
    let journey_ids = unique_ids(
        state.active_journeys.iter().map(|journey| journey.id),
        "Journey",
    )?;
    if state
        .active_journeys
        .iter()
        .any(|journey| journey.id.get() == 0)
    {
        return Err(SaveValidationError::InvalidId { kind: "Journey" });
    }

    validate_train_statuses(
        &state.player_company.fleet.trains,
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SaveEnvelope<T = GameState> {
    version: u32,
    state: T,
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
    if balance.diesel_catalogue().is_empty() {
        return Err(SaveValidationError::InvalidValue {
            field: "diesel Train catalogue",
        });
    }
    if balance
        .diesel_catalogue()
        .iter()
        .any(|record| record.name().trim().is_empty() || record.purchase_price().cents() <= 0)
    {
        return Err(SaveValidationError::InvalidValue {
            field: "diesel Train catalogue record",
        });
    }
    if state.rules.demand.cap_duration.seconds() == 0 {
        return Err(SaveValidationError::InvalidValue {
            field: "demand cap duration",
        });
    }
    Ok(())
}

fn service_distance(
    service: &PassengerService,
    network: &RailNetwork,
    line_ids: &HashSet<RailLineId>,
) -> Result<DistanceMetres, SaveValidationError> {
    if service.first_station_id == service.second_station_id || service.rail_line_ids.is_empty() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Passenger Service must have distinct endpoints and a Rail Line path",
        });
    }

    let mut current_station_id = service.first_station_id;
    let mut total_metres = 0_u64;
    let mut used_line_ids = HashSet::new();
    for rail_line_id in &service.rail_line_ids {
        if !line_ids.contains(rail_line_id) {
            return Err(SaveValidationError::DanglingReference {
                field: "Passenger Service Rail Line",
            });
        }
        if !used_line_ids.insert(*rail_line_id) {
            return Err(SaveValidationError::ImpossibleState {
                reason: "a Passenger Service repeats a Rail Line",
            });
        }
        let line = network
            .rail_lines
            .iter()
            .find(|line| line.id == *rail_line_id)
            .expect("a validated Rail Line ID resolves in the Rail Network");
        current_station_id = next_station_on_line(line, current_station_id).ok_or(
            SaveValidationError::ImpossibleState {
                reason: "Passenger Service Rail Lines do not form a continuous path",
            },
        )?;
        total_metres = total_metres.checked_add(line.distance.metres()).ok_or(
            SaveValidationError::Calculation(CalculationError::Overflow {
                operation: "Passenger Service path distance",
            }),
        )?;
    }
    if current_station_id != service.second_station_id {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Passenger Service Rail Line path does not end at its endpoint",
        });
    }
    let metres = i64::try_from(total_metres).map_err(|_| {
        SaveValidationError::Calculation(CalculationError::Overflow {
            operation: "Passenger Service path distance",
        })
    })?;
    DistanceMetres::new(metres).map_err(|_| SaveValidationError::InvalidValue {
        field: "Passenger Service path distance",
    })
}

fn next_station_on_line(line: &RailLine, station_id: RailStationId) -> Option<RailStationId> {
    if line.first_station_id == station_id {
        Some(line.second_station_id)
    } else if line.second_station_id == station_id {
        Some(line.first_station_id)
    } else {
        None
    }
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
        let cap = (u128::from(demand.passenger_arrival_rate_per_hour.passengers_per_hour())
            * u128::from(state.rules.demand.cap_duration.seconds())
            / 3_600)
            .min(u128::from(u32::MAX)) as u32;
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
        if receipt.journey_id.get() == 0 || !receipt_ids.insert(receipt.journey_id) {
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
            let train_id = receipt.train_id.expect("complete receipt context has Train ID");
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
            let passengers = receipt
                .passengers_carried
                .expect("complete receipt context has passengers");
            let capacity = receipt
                .passenger_capacity
                .expect("complete receipt context has capacity");
            if train_id.get() == 0
                || train_model_name.trim().is_empty()
                || origin == destination
                || !station_ids.contains(&origin)
                || !station_ids.contains(&destination)
                || capacity == 0
                || passengers > capacity
            {
                return Err(SaveValidationError::InvalidValue {
                    field: "Journey receipt operating context",
                });
            }
        }
    }
    Ok(())
}

fn validate_train_statuses(
    trains: &[Train],
    station_ids: &HashSet<RailStationId>,
    journey_ids: &HashSet<crate::model::JourneyId>,
) -> Result<(), SaveValidationError> {
    let mut travelling_journey_ids = HashSet::new();
    for train in trains {
        if train.model_name.trim().is_empty() || train.original_purchase_price.cents() <= 0 {
            return Err(SaveValidationError::InvalidValue {
                field: "Train catalogue data",
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
    if journey.passengers_carried > train.passenger_capacity.passengers() {
        return Err(SaveValidationError::ImpossibleState {
            reason: "a Journey carries more passengers than its Train capacity",
        });
    }
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .expect("a validated Journey Passenger Service ID resolves in the Service Network");
    let valid_direction = (journey.origin_station_id == service.first_station_id
        && journey.destination_station_id == service.second_station_id)
        || (journey.origin_station_id == service.second_station_id
            && journey.destination_station_id == service.first_station_id);
    if !valid_direction {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey endpoints do not match its Passenger Service",
        });
    }
    let distance = service_distances
        .get(&journey.service_id)
        .copied()
        .expect("every validated Passenger Service has a calculated distance");
    let expected_fare = state
        .rules
        .balance
        .fare_per_passenger_kilometre()
        .checked_charge(distance)?;
    let expected_revenue = expected_fare.checked_mul(u64::from(journey.passengers_carried))?;
    let expected_access_fee = state
        .rules
        .balance
        .access_fee_per_train_kilometre()
        .checked_charge(distance)?;
    let expected_fuel_cost = train.fuel_cost_per_kilometre.checked_charge(distance)?;
    expected_access_fee.checked_add(expected_fuel_cost)?;
    let expected_duration = distance.journey_duration(train.speed)?;
    let expected_arrival = journey.departed_at.checked_add(expected_duration)?;
    if journey.fare != expected_fare
        || journey.operating_revenue != expected_revenue
        || journey.infrastructure_access_fee != expected_access_fee
        || journey.fuel_cost != expected_fuel_cost
        || journey.arrives_at != expected_arrival
    {
        return Err(SaveValidationError::ImpossibleState {
            reason: "Journey actuals do not match its saved rules and Passenger Service",
        });
    }

    // Quoting against a temporarily READY copy also verifies that the Journey's
    // direction has a live directional demand pool without admitting it first.
    let mut quote_candidate = state.clone();
    quote_candidate
        .player_company
        .fleet
        .trains
        .iter_mut()
        .find(|candidate| candidate.id == journey.train_id)
        .expect("validated Journey Train remains present in quote candidate")
        .status = TrainStatus::Ready {
        at: journey.origin_station_id,
    };
    quote_journey(&quote_candidate, journey.train_id, journey.service_id).map_err(|_| {
        SaveValidationError::ImpossibleState {
            reason: "Journey cannot be quoted from its saved Passenger Service",
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::{
        model::{DistanceMetres, DurationSeconds, RailLineId, TrainId, TrainStatus, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            time::advance_time, world::create_new_game,
        },
    };

    use super::*;

    static NEXT_TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "railq-storage-test-{}-{}",
                std::process::id(),
                NEXT_TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn save_path(&self) -> PathBuf {
            self.path.join("company.ron")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn active_game() -> GameState {
        let departed_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
        state.origin_destination_demand[0].fractional_passenger_seconds = 1_234;
        state
    }

    fn raw_save(state: &GameState, version: u32) -> String {
        ron::ser::to_string(&SaveEnvelope { version, state }).unwrap()
    }

    #[test]
    fn round_trips_all_current_operating_state() {
        let state = active_game();
        let encoded = encode_game_state(&state).unwrap();

        assert_eq!(decode_game_state(&encoded).unwrap(), state);
    }

    #[test]
    fn round_trips_enriched_journey_receipts() {
        let mut state = active_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).unwrap();

        let encoded = encode_game_state(&state).unwrap();
        let decoded = decode_game_state(&encoded).unwrap();

        assert_eq!(decoded, state);
        let receipt = &decoded.financials.recent_journey_receipts[0];
        assert!(receipt.train_id.is_some());
        assert!(receipt.train_model_name.is_some());
        assert!(receipt.origin_station_id.is_some());
        assert!(receipt.destination_station_id.is_some());
        assert!(receipt.passengers_carried.is_some());
        assert!(receipt.passenger_capacity.is_some());
        assert_eq!(receipt.completed_at, Some(arrives_at));
    }

    #[test]
    fn rejects_an_unsupported_save_version() {
        let source = raw_save(&active_game(), SAVE_VERSION + 1);

        assert_eq!(
            decode_game_state(&source),
            Err(SaveCodecError::UnsupportedVersion {
                found: SAVE_VERSION + 1
            })
        );
    }

    #[test]
    fn rejects_dangling_references_and_contradictory_train_status() {
        let mut state = active_game();
        state.player_company.passenger_services[0].rail_line_ids = vec![RailLineId::new(99)];
        assert!(matches!(
            decode_game_state(&raw_save(&state, SAVE_VERSION)),
            Err(SaveCodecError::InvalidState(
                SaveValidationError::DanglingReference { .. }
            ))
        ));

        let mut state = active_game();
        state.player_company.fleet.trains[0].status = TrainStatus::Ready {
            at: RailStationId::new(1),
        };
        assert!(matches!(
            decode_game_state(&raw_save(&state, SAVE_VERSION)),
            Err(SaveCodecError::InvalidState(
                SaveValidationError::ImpossibleState { .. }
            ))
        ));

        let mut state = active_game();
        let mut duplicate_owner = state.player_company.fleet.trains[0].clone();
        duplicate_owner.id = TrainId::new(2);
        state.player_company.fleet.trains.push(duplicate_owner);
        assert!(matches!(
            decode_game_state(&raw_save(&state, SAVE_VERSION)),
            Err(SaveCodecError::InvalidState(
                SaveValidationError::ImpossibleState { .. }
            ))
        ));
    }

    #[test]
    fn rejects_invalid_numbers_and_arithmetic_domain_violations() {
        let mut state = active_game();
        state.rules.demand.cap_duration = DurationSeconds::from_seconds(0);
        assert!(matches!(
            decode_game_state(&raw_save(&state, SAVE_VERSION)),
            Err(SaveCodecError::InvalidState(
                SaveValidationError::InvalidValue { .. }
            ))
        ));

        let mut state = active_game();
        state.region.rail_authority.rail_network.rail_lines[0].distance =
            DistanceMetres::new(i64::MAX).unwrap();
        assert!(matches!(
            decode_game_state(&raw_save(&state, SAVE_VERSION)),
            Err(SaveCodecError::InvalidState(
                SaveValidationError::Calculation(_)
            ))
        ));
    }

    #[test]
    fn missing_save_file_is_reported_without_creating_a_fresh_game() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();

        assert_eq!(slot.load().unwrap(), None);
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_save_is_preserved_and_cannot_be_replaced() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let corrupt_contents = "this is not valid RON";
        fs::write(&path, corrupt_contents).unwrap();
        let slot = SaveSlot::open(&path).unwrap();

        assert!(matches!(
            slot.load(),
            Err(SaveSlotError::InvalidSave { .. })
        ));
        assert!(matches!(
            slot.save(&active_game()),
            Err(SaveSlotError::InvalidSave { .. })
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), corrupt_contents);
    }

    #[test]
    fn second_opener_cannot_own_the_same_save_slot() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let _first_slot = SaveSlot::open(&path).unwrap();

        assert!(matches!(
            SaveSlot::open(path),
            Err(SaveSlotError::AlreadyOwned { .. })
        ));
    }

    #[test]
    fn failed_write_leaves_the_old_valid_save_intact() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();
        let state = active_game();
        slot.save(&state).unwrap();
        let old_contents = fs::read_to_string(&path).unwrap();

        assert!(matches!(
            slot.save_with_before_replace(&state, |_| {
                Err(io::Error::other("simulated write failure"))
            }),
            Err(SaveSlotError::Io { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), old_contents);
        assert_eq!(slot.load().unwrap(), Some(state));
    }

    #[test]
    fn restart_backup_preserves_the_old_save_before_replacing_it() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();
        let old_state = active_game();
        slot.save(&old_state).unwrap();
        let old_contents = fs::read_to_string(&path).unwrap();
        let replacement = create_new_game(99, "New Passenger", UtcSeconds::from_unix_seconds(2));

        let backup_path = slot.save_after_backup(&replacement).unwrap();

        assert!(backup_path.exists());
        assert_eq!(fs::read_to_string(&backup_path).unwrap(), old_contents);
        assert_eq!(slot.load().unwrap(), Some(replacement));
        assert_ne!(fs::read_to_string(&path).unwrap(), old_contents);
    }

    #[test]
    fn successful_write_uses_a_same_directory_temporary_before_replacement() {
        let directory = TestDirectory::new();
        let path = directory.save_path();
        let slot = SaveSlot::open(&path).unwrap();
        let state = active_game();
        slot.save(&state).unwrap();
        let old_contents = fs::read_to_string(&path).unwrap();
        let mut replacement = state.clone();
        replacement.player_company.name.push_str(" Renewed");

        slot.save_with_before_replace(&replacement, |temporary_path| {
            assert_eq!(temporary_path.parent(), Some(directory.path.as_path()));
            assert_eq!(fs::read_to_string(&path).unwrap(), old_contents);
            Ok(())
        })
        .unwrap();

        assert_eq!(slot.load().unwrap(), Some(replacement));
        assert!(fs::read_dir(&directory.path).unwrap().all(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !(name.starts_with(".company.ron.") && name.ends_with(".tmp"))
        }));
    }
}

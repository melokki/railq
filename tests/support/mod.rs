use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use railq::{
    model::{GameState, UtcSeconds},
    sim::world::create_new_game,
};

pub const DEFAULT_TEST_SEED: u64 = 42;

static NEXT_TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

/// Creates a deterministic game fixture using the repository-wide test seed.
pub fn new_game(company_name: &str, started_at: UtcSeconds) -> GameState {
    create_new_game(DEFAULT_TEST_SEED, company_name, started_at)
}

/// Owns an isolated temporary directory and removes it when the test finishes.
pub struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "railq-{prefix}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("test directory is created");
        Self { path }
    }

    pub fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

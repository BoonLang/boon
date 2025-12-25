//! File-based persistence for CLI.

use std::path::PathBuf;
use std::fs;

pub struct FileStorage {
    base_path: PathBuf,
}

impl FileStorage {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    pub fn load(&self, key: &str) -> Option<String> {
        let path = self.base_path.join(format!("{}.json", key));
        fs::read_to_string(path).ok()
    }

    pub fn save(&self, key: &str, value: &str) -> std::io::Result<()> {
        fs::create_dir_all(&self.base_path)?;
        let path = self.base_path.join(format!("{}.json", key));
        fs::write(path, value)
    }

    pub fn remove(&self, key: &str) -> std::io::Result<()> {
        let path = self.base_path.join(format!("{}.json", key));
        fs::remove_file(path)
    }
}

use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub token_file: PathBuf,
    pub cache_dir: PathBuf,
}

impl Config {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("com", "remarkable-mcp", "remarkable-mcp")
            .ok_or_else(|| Error::InvalidInput("could not determine config directory".into()))?;
        Ok(Self {
            token_file: dirs.config_dir().join("token.json"),
            cache_dir: dirs.cache_dir().join("blobs"),
        })
    }
}

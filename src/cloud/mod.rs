//! Client for the reMarkable cloud API.
//!
//! `CloudClient` is the public façade; protocol details are kept in focused
//! modules so authentication, storage transport, and document operations can
//! evolve independently.

mod auth;
mod index;
mod library;
mod transport;
mod write;

use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use crate::{config::Config, error::Result, model::Library};

pub use auth::TokenData;
use auth::parse_token;

const ROOT_URL: &str = "https://internal.cloud.remarkable.com/sync/v4/root";
const ROOT_PUT_URL: &str = "https://internal.cloud.remarkable.com/sync/v3/root";
const FILES_URL: &str = "https://internal.cloud.remarkable.com/sync/v3/files";
const MAX_ROOT_ATTEMPTS: usize = 5;

#[derive(Debug, Deserialize)]
struct RootResponse {
    hash: String,
    #[serde(default)]
    generation: u64,
}

#[derive(Debug, Clone)]
enum RequestBody {
    Bytes(Vec<u8>),
    Json(Value),
}

#[derive(Clone)]
pub struct CloudClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: Config,
    tokens: Mutex<TokenData>,
    library: RwLock<Option<Library>>,
}

impl CloudClient {
    pub async fn load(config: Config) -> Result<Self> {
        let tokens = if let Ok(value) = std::env::var("REMARKABLE_TOKEN") {
            parse_token(&value)?
        } else if config.token_file.is_file() {
            parse_token(&tokio::fs::read_to_string(&config.token_file).await?)?
        } else {
            TokenData::default()
        };
        let http = reqwest::Client::builder()
            .user_agent(concat!("remarkable-mcp/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                tokens: Mutex::new(tokens),
                library: RwLock::new(None),
            }),
        })
    }
}

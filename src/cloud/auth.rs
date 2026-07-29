use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::CloudClient;
use crate::error::{Error, Result};

const DEVICE_TOKEN_URL: &str =
    "https://webapp-prod.cloud.remarkable.engineering/token/json/2/device/new";
const USER_TOKEN_URL: &str =
    "https://webapp-prod.cloud.remarkable.engineering/token/json/2/user/new";
const CONNECT_URL: &str = "https://my.remarkable.com/device/desktop/connect";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenData {
    #[serde(default)]
    pub devicetoken: String,
    #[serde(default)]
    pub usertoken: String,
}

impl CloudClient {
    pub const fn connect_url() -> &'static str {
        CONNECT_URL
    }

    pub async fn register(&self, code: &str) -> Result<()> {
        let code = code.trim();
        if code.is_empty() {
            return Err(Error::InvalidInput("connection code is empty".into()));
        }
        let response = self
            .inner
            .http
            .post(DEVICE_TOKEN_URL)
            .json(&json!({
                "code": code,
                "deviceDesc": device_description(),
                "deviceID": Uuid::new_v4().to_string(),
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Cloud("connection code is invalid or expired".into()));
        }
        let device_token = response.text().await?.trim().to_owned();
        if device_token.is_empty() {
            return Err(Error::Cloud("registration returned an empty token".into()));
        }
        let tokens = TokenData {
            devicetoken: device_token,
            usertoken: String::new(),
        };
        self.save_tokens(&tokens).await?;
        *self.inner.tokens.lock().await = tokens;
        *self.inner.library.write().await = None;
        self.ensure_user_token().await?;
        Ok(())
    }

    pub async fn is_configured(&self) -> bool {
        !self.inner.tokens.lock().await.devicetoken.is_empty()
    }

    pub(super) async fn ensure_user_token(&self) -> Result<String> {
        let mut tokens = self.inner.tokens.lock().await;
        if !tokens.usertoken.is_empty() {
            return Ok(tokens.usertoken.clone());
        }
        if tokens.devicetoken.is_empty() {
            return Err(Error::NotConnected);
        }
        let response = self
            .inner
            .http
            .post(USER_TOKEN_URL)
            .bearer_auth(&tokens.devicetoken)
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Cloud(
                "device registration is no longer valid".into(),
            ));
        }
        tokens.usertoken = response.text().await?.trim().to_owned();
        self.save_tokens(&tokens).await?;
        Ok(tokens.usertoken.clone())
    }

    async fn save_tokens(&self, tokens: &TokenData) -> Result<()> {
        let parent = self
            .inner
            .config
            .token_file
            .parent()
            .ok_or_else(|| Error::InvalidInput("invalid token path".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&self.inner.config.token_file, serde_json::to_vec(tokens)?).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(
                &self.inner.config.token_file,
                std::fs::Permissions::from_mode(0o600),
            )
            .await?;
        }
        Ok(())
    }
}

pub(super) fn parse_token(value: &str) -> Result<TokenData> {
    if value.trim_start().starts_with('{') {
        return Ok(serde_json::from_str(value)?);
    }
    Ok(TokenData {
        devicetoken: value.trim().into(),
        usertoken: String::new(),
    })
}

fn device_description() -> &'static str {
    if cfg!(target_os = "macos") {
        "desktop-macos"
    } else if cfg!(target_os = "windows") {
        "desktop-windows"
    } else {
        "desktop-linux"
    }
}

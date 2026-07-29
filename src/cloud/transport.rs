use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Method, Response, StatusCode};

use super::{CloudClient, FILES_URL, RequestBody};
use crate::error::{Error, Result};

impl CloudClient {
    pub(super) async fn put_blob(
        &self,
        bytes: Vec<u8>,
        hash: &str,
        filename: &str,
        content_type: &str,
    ) -> Result<()> {
        let checksum = format!(
            "crc32c={}",
            STANDARD.encode(crc32c::crc32c(&bytes).to_be_bytes())
        );
        self.send(
            Method::PUT,
            &format!("{FILES_URL}/{hash}"),
            vec![
                ("rm-filename", filename.into()),
                ("x-goog-hash", checksum),
                ("content-type", content_type.into()),
            ],
            Some(RequestBody::Bytes(bytes.clone())),
        )
        .await?;
        self.cache_write(hash, &bytes).await;
        Ok(())
    }

    pub(super) async fn get_file(&self, hash: &str, filename: &str) -> Result<Vec<u8>> {
        if !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Cloud("invalid content hash".into()));
        }
        let path = self.inner.config.cache_dir.join(hash);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let bytes = self
            .send(
                Method::GET,
                &format!("{FILES_URL}/{hash}"),
                vec![("rm-filename", filename.into())],
                None,
            )
            .await?
            .bytes()
            .await?
            .to_vec();
        self.cache_write(hash, &bytes).await;
        Ok(bytes)
    }

    async fn cache_write(&self, hash: &str, bytes: &[u8]) {
        if bytes.len() > 8 * 1024 * 1024 {
            return;
        }
        if tokio::fs::create_dir_all(&self.inner.config.cache_dir)
            .await
            .is_ok()
        {
            let _ = tokio::fs::write(self.inner.config.cache_dir.join(hash), bytes).await;
        }
    }

    pub(super) async fn send(
        &self,
        method: Method,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: Option<RequestBody>,
    ) -> Result<Response> {
        ensure_success(self.send_allow_conflict(method, url, headers, body).await?).await
    }

    pub(super) async fn send_allow_conflict(
        &self,
        method: Method,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: Option<RequestBody>,
    ) -> Result<Response> {
        let mut renewed = false;
        for attempt in 0..3 {
            let token = self.ensure_user_token().await?;
            let mut request = self
                .inner
                .http
                .request(method.clone(), url)
                .bearer_auth(token);
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            request = match &body {
                Some(RequestBody::Bytes(bytes)) => request.body(bytes.clone()),
                Some(RequestBody::Json(value)) => request.json(value),
                None => request,
            };
            match request.send().await {
                Ok(response) if response.status() == StatusCode::UNAUTHORIZED && !renewed => {
                    self.inner.tokens.lock().await.usertoken.clear();
                    renewed = true;
                }
                Ok(response) if is_retryable(response.status()) && attempt < 2 => {
                    backoff(attempt).await
                }
                Ok(response) => return Ok(response),
                Err(error) if (error.is_connect() || error.is_timeout()) && attempt < 2 => {
                    backoff(attempt).await
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Cloud("request retries exhausted".into()))
    }
}

pub(super) async fn ensure_success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let message = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect::<String>();
    Err(Error::Cloud(format!("HTTP {status}: {message}")))
}

fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

pub(super) async fn backoff(attempt: usize) {
    tokio::time::sleep(Duration::from_millis(
        150_u64.saturating_mul(1_u64 << attempt.min(5)),
    ))
    .await;
}

use std::{path::Path, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, TimeZone, Utc};
use futures_util::{StreamExt, stream};
use reqwest::{Method, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    config::Config,
    error::{Error, Result},
    model::{BlobEntry, Item, ItemKind, Library, MAX_UPLOAD_BYTES},
};

const DEVICE_TOKEN_URL: &str =
    "https://webapp-prod.cloud.remarkable.engineering/token/json/2/device/new";
const USER_TOKEN_URL: &str =
    "https://webapp-prod.cloud.remarkable.engineering/token/json/2/user/new";
const ROOT_URL: &str = "https://internal.cloud.remarkable.com/sync/v4/root";
const ROOT_PUT_URL: &str = "https://internal.cloud.remarkable.com/sync/v3/root";
const FILES_URL: &str = "https://internal.cloud.remarkable.com/sync/v3/files";
const CONNECT_URL: &str = "https://my.remarkable.com/device/desktop/connect";
const MAX_ROOT_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenData {
    #[serde(default)]
    pub devicetoken: String,
    #[serde(default)]
    pub usertoken: String,
}

#[derive(Debug, Deserialize)]
struct RootResponse {
    hash: String,
    #[serde(default)]
    generation: u64,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(rename = "visibleName")]
    visible_name: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    parent: String,
    #[serde(default)]
    deleted: bool,
    #[serde(rename = "lastModified")]
    last_modified: Option<String>,
    #[serde(default)]
    tags: Vec<Value>,
}

#[derive(Debug, Clone)]
enum RequestBody {
    Bytes(Vec<u8>),
    Json(Value),
}

#[derive(Debug, Clone)]
enum MetadataChange {
    Rename(String),
    Move(String),
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

    pub async fn check_connection(&self) -> Result<()> {
        self.send(Method::GET, ROOT_URL, Vec::new(), None).await?;
        Ok(())
    }

    pub async fn library(&self, refresh: bool) -> Result<Library> {
        if !refresh && let Some(library) = self.inner.library.read().await.clone() {
            return Ok(library);
        }
        let root: RootResponse = self
            .send(Method::GET, ROOT_URL, Vec::new(), None)
            .await?
            .json()
            .await?;
        let entries = parse_index(&self.get_file(&root.hash, "root.docSchema").await?)?;
        let client = self.clone();
        let mut items = stream::iter(entries.into_iter().map(move |entry| {
            let client = client.clone();
            async move { client.load_item(entry).await }
        }))
        .buffer_unordered(16)
        .filter_map(|result| async move {
            match result {
                Ok(Some(item)) => Some(Ok(item)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Vec<Result<Item>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        items.sort_by_key(|item| item.name.to_lowercase());
        let library = Library::new(items);
        *self.inner.library.write().await = Some(library.clone());
        Ok(library)
    }

    async fn load_item(&self, root_entry: BlobEntry) -> Result<Option<Item>> {
        let index = self
            .get_file(&root_entry.hash, &format!("{}.docSchema", root_entry.id))
            .await?;
        let files = parse_index(&index)?;
        let Some(meta) = files.iter().find(|entry| entry.id.ends_with(".metadata")) else {
            return Ok(None);
        };
        let metadata: Metadata =
            serde_json::from_slice(&self.get_file(&meta.hash, &meta.id).await?)?;
        if metadata.deleted {
            return Ok(None);
        }
        let tags = extract_tags(&metadata.tags);
        let kind = if metadata.item_type.as_deref() == Some("CollectionType") {
            ItemKind::Folder
        } else if files.iter().any(|entry| entry.id.ends_with(".pdf")) {
            ItemKind::Pdf
        } else if files.iter().any(|entry| entry.id.ends_with(".epub")) {
            ItemKind::Epub
        } else {
            ItemKind::Notebook
        };
        Ok(Some(Item {
            id: root_entry.id.clone(),
            hash: root_entry.hash,
            name: metadata.visible_name.unwrap_or(root_entry.id),
            parent: metadata.parent,
            kind,
            modified_at: parse_modified(metadata.last_modified.as_deref()),
            size: root_entry.size,
            files,
            tags,
        }))
    }

    pub async fn resolve(&self, reference: &str, folder_only: bool) -> Result<(Library, usize)> {
        let library = self.library(false).await?;
        let needle = normalize_path(reference);
        if let Some(index) = library.paths.get(&needle.to_lowercase()).copied()
            && (!folder_only || library.items[index].is_folder())
        {
            return Ok((library, index));
        }
        let matches = library
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (!folder_only || item.is_folder()) && item.name.eq_ignore_ascii_case(reference)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => Ok((library, *index)),
            [] => Err(Error::NotFound(reference.into())),
            _ => Err(Error::Ambiguous(reference.into())),
        }
    }

    pub async fn source_bytes(&self, item: &Item) -> Result<Vec<u8>> {
        let suffix = match item.kind {
            ItemKind::Pdf => ".pdf",
            ItemKind::Epub => ".epub",
            _ => return Err(Error::Unsupported("document has no source file".into())),
        };
        let entry = item
            .files
            .iter()
            .find(|entry| entry.id.ends_with(suffix))
            .ok_or_else(|| Error::Unsupported(format!("{} source is unavailable", item.name)))?;
        self.get_file(&entry.hash, &entry.id).await
    }

    pub async fn file_ending_with(&self, item: &Item, suffix: &str) -> Result<Option<Vec<u8>>> {
        let Some(entry) = item.files.iter().find(|entry| entry.id.ends_with(suffix)) else {
            return Ok(None);
        };
        self.get_file(&entry.hash, &entry.id).await.map(Some)
    }

    pub async fn document_archive(&self, item: &Item) -> Result<Vec<(String, Vec<u8>)>> {
        let mut result = Vec::with_capacity(item.files.len());
        for entry in &item.files {
            result.push((
                entry.id.clone(),
                self.get_file(&entry.hash, &entry.id).await?,
            ));
        }
        Ok(result)
    }

    pub async fn create_folder(&self, name: &str, parent_id: &str) -> Result<Item> {
        validate_name(name)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        let metadata = serde_json::to_vec(&json!({
            "createdTime": now, "lastModified": now, "new": false,
            "parent": parent_id, "pinned": false, "source": "",
            "type": "CollectionType", "visibleName": name,
        }))?;
        let file = self
            .upload_file_blob(metadata, &format!("{id}.metadata"))
            .await?;
        let document = self.upload_document_index(&id, vec![file]).await?;
        self.append_root_entry(document).await?;
        self.item_after_write(&id).await
    }

    pub async fn rename(&self, item_id: &str, name: &str) -> Result<Item> {
        validate_name(name)?;
        self.change_metadata(item_id, MetadataChange::Rename(name.into()))
            .await
    }

    pub async fn move_item(&self, item_id: &str, parent_id: &str) -> Result<Item> {
        self.change_metadata(item_id, MetadataChange::Move(parent_id.into()))
            .await
    }

    pub async fn delete(&self, item_id: &str) -> Result<Item> {
        self.move_item(item_id, "trash").await
    }

    async fn change_metadata(&self, item_id: &str, change: MetadataChange) -> Result<Item> {
        for attempt in 0..MAX_ROOT_ATTEMPTS {
            let (entries, generation) = self.read_root_entries().await?;
            let existing = entries
                .iter()
                .find(|entry| entry.id == item_id)
                .cloned()
                .ok_or_else(|| Error::NotFound(item_id.into()))?;
            let new_entry = self.rewrite_metadata(&existing, &change).await?;
            let updated = entries
                .into_iter()
                .map(|entry| {
                    if entry.id == item_id {
                        new_entry.clone()
                    } else {
                        entry
                    }
                })
                .collect();
            if self.commit_entries(updated, generation).await? {
                return self.item_after_write(item_id).await;
            }
            backoff(attempt).await;
        }
        Err(Error::Cloud("could not commit change after retries".into()))
    }

    async fn rewrite_metadata(
        &self,
        root: &BlobEntry,
        change: &MetadataChange,
    ) -> Result<BlobEntry> {
        let mut files = parse_index(
            &self
                .get_file(&root.hash, &format!("{}.docSchema", root.id))
                .await?,
        )?;
        let index = files
            .iter()
            .position(|entry| entry.id.ends_with(".metadata"))
            .ok_or_else(|| Error::Cloud("document metadata is missing".into()))?;
        let mut metadata: Value =
            serde_json::from_slice(&self.get_file(&files[index].hash, &files[index].id).await?)?;
        match change {
            MetadataChange::Rename(name) => metadata["visibleName"] = json!(name),
            MetadataChange::Move(parent) => metadata["parent"] = json!(parent),
        }
        metadata["lastModified"] = json!(now_ms());
        metadata["metadatamodified"] = json!(true);
        metadata["version"] = json!(metadata["version"].as_u64().unwrap_or(0) + 1);
        let filename = files[index].id.clone();
        files[index] = self
            .upload_file_blob(serde_json::to_vec(&metadata)?, &filename)
            .await?;
        self.upload_document_index(&root.id, files).await
    }

    pub async fn upload(
        &self,
        path: &Path,
        parent_id: &str,
        requested_name: Option<&str>,
    ) -> Result<Item> {
        let file_metadata = tokio::fs::metadata(path).await?;
        if file_metadata.len() > MAX_UPLOAD_BYTES {
            return Err(Error::InvalidInput("upload exceeds 512 MiB".into()));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_lowercase)
            .ok_or_else(|| Error::InvalidInput("file must be PDF or EPUB".into()))?;
        if extension != "pdf" && extension != "epub" {
            return Err(Error::InvalidInput("file must be PDF or EPUB".into()));
        }
        let bytes = tokio::fs::read(path).await?;
        let name = requested_name
            .map(str::to_owned)
            .or_else(|| path.file_stem()?.to_str().map(str::to_owned))
            .ok_or_else(|| Error::InvalidInput("could not determine document name".into()))?;
        validate_name(&name)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        let page_count = if extension == "pdf" {
            lopdf::Document::load_mem(&bytes)
                .map(|pdf| pdf.get_pages().len())
                .unwrap_or(0)
        } else {
            0
        };
        let pages = (0..page_count)
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        let content = serde_json::to_vec(&json!({
            "coverPageNumber": -1, "documentMetadata": {}, "dummyDocument": false,
            "extraMetadata": {}, "fileType": extension, "fontName": "",
            "formatVersion": 1, "lineHeight": -1, "margins": 100,
            "orientation": "portrait", "pageCount": page_count, "pageTags": [],
            "pages": pages, "tags": [], "textScale": 1,
        }))?;
        let metadata = serde_json::to_vec(&json!({
            "createdTime": now, "deleted": false, "lastModified": now,
            "lastOpened": now, "lastOpenedPage": 0, "metadatamodified": false,
            "modified": false, "new": false, "parent": parent_id, "pinned": false,
            "source": "", "synced": false, "type": "DocumentType", "version": 1,
            "visibleName": name,
        }))?;
        let pagedata = if page_count == 0 {
            vec![b'\n']
        } else {
            "Blank\n".repeat(page_count).into_bytes()
        };
        let files = vec![
            self.upload_file_blob(content, &format!("{id}.content"))
                .await?,
            self.upload_file_blob(metadata, &format!("{id}.metadata"))
                .await?,
            self.upload_file_blob(pagedata, &format!("{id}.pagedata"))
                .await?,
            self.upload_file_blob(bytes, &format!("{id}.{extension}"))
                .await?,
        ];
        let document = self.upload_document_index(&id, files).await?;
        self.append_root_entry(document).await?;
        self.item_after_write(&id).await
    }

    async fn append_root_entry(&self, entry: BlobEntry) -> Result<()> {
        for attempt in 0..MAX_ROOT_ATTEMPTS {
            let (mut entries, generation) = self.read_root_entries().await?;
            if !entries.iter().any(|existing| existing.id == entry.id) {
                entries.push(entry.clone());
            }
            if self.commit_entries(entries, generation).await? {
                return Ok(());
            }
            backoff(attempt).await;
        }
        Err(Error::Cloud("could not commit write after retries".into()))
    }

    async fn item_after_write(&self, id: &str) -> Result<Item> {
        *self.inner.library.write().await = None;
        let library = self.library(true).await?;
        let index = library
            .by_id
            .get(id)
            .copied()
            .ok_or_else(|| Error::NotFound(id.into()))?;
        Ok(library.items[index].clone())
    }

    async fn read_root_entries(&self) -> Result<(Vec<BlobEntry>, u64)> {
        let root: RootResponse = self
            .send(Method::GET, ROOT_URL, Vec::new(), None)
            .await?
            .json()
            .await?;
        Ok((
            parse_index(&self.get_file(&root.hash, "root.docSchema").await?)?,
            root.generation,
        ))
    }

    async fn commit_entries(&self, entries: Vec<BlobEntry>, generation: u64) -> Result<bool> {
        let body = serialize_root_index(&entries);
        let hash = sha256_hex(&body);
        self.put_blob(body, &hash, "root.docSchema", "text/plain; charset=UTF-8")
            .await?;
        let response = self
            .send_allow_conflict(
                Method::PUT,
                ROOT_PUT_URL,
                vec![("rm-filename", "roothash".into())],
                Some(RequestBody::Json(
                    json!({ "broadcast": true, "hash": hash, "generation": generation }),
                )),
            )
            .await?;
        if matches!(
            response.status(),
            StatusCode::CONFLICT
                | StatusCode::PRECONDITION_FAILED
                | StatusCode::PRECONDITION_REQUIRED
        ) {
            return Ok(false);
        }
        ensure_success(response).await?;
        Ok(true)
    }

    async fn upload_file_blob(&self, bytes: Vec<u8>, name: &str) -> Result<BlobEntry> {
        let hash = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        self.put_blob(bytes, &hash, name, "application/octet-stream")
            .await?;
        Ok(BlobEntry {
            hash,
            id: name.into(),
            subfiles: 0,
            size,
        })
    }

    async fn upload_document_index(&self, id: &str, files: Vec<BlobEntry>) -> Result<BlobEntry> {
        let bytes = serialize_document_index(&files);
        let hash = hash_entries(&files)?;
        self.put_blob(bytes, &hash, &format!("{id}.docSchema"), "text/plain")
            .await?;
        Ok(BlobEntry {
            hash,
            id: id.into(),
            subfiles: files.len() as u64,
            size: files.iter().map(|entry| entry.size).sum(),
        })
    }

    async fn put_blob(
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

    async fn get_file(&self, hash: &str, filename: &str) -> Result<Vec<u8>> {
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

    async fn send(
        &self,
        method: Method,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: Option<RequestBody>,
    ) -> Result<Response> {
        ensure_success(self.send_allow_conflict(method, url, headers, body).await?).await
    }

    async fn send_allow_conflict(
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

    async fn ensure_user_token(&self) -> Result<String> {
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

fn parse_token(value: &str) -> Result<TokenData> {
    if value.trim_start().starts_with('{') {
        return Ok(serde_json::from_str(value)?);
    }
    Ok(TokenData {
        devicetoken: value.trim().into(),
        usertoken: String::new(),
    })
}

fn parse_index(bytes: &[u8]) -> Result<Vec<BlobEntry>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::Cloud("cloud index is not UTF-8".into()))?;
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty() && !line.starts_with("0:."))
        .map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            if fields.len() < 5 {
                return Err(Error::Cloud("malformed cloud index".into()));
            }
            Ok(BlobEntry {
                hash: fields[0].into(),
                id: fields[2].into(),
                subfiles: fields[3]
                    .parse()
                    .map_err(|_| Error::Cloud("invalid subfile count".into()))?,
                size: fields[4]
                    .parse()
                    .map_err(|_| Error::Cloud("invalid blob size".into()))?,
            })
        })
        .collect()
}

fn serialize_document_index(entries: &[BlobEntry]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let mut output = String::from("3\n");
    for entry in sorted {
        output.push_str(&format!("{}:0:{}:0:{}\n", entry.hash, entry.id, entry.size));
    }
    output.into_bytes()
}

fn serialize_root_index(entries: &[BlobEntry]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let total = sorted.iter().map(|entry| entry.size).sum::<u64>();
    let mut output = format!("4\n0:.:{}:{}\n", sorted.len(), total);
    for entry in sorted {
        output.push_str(&format!(
            "{}:0:{}:{}:{}\n",
            entry.hash, entry.id, entry.subfiles, entry.size
        ));
    }
    output.into_bytes()
}

fn hash_entries(entries: &[BlobEntry]) -> Result<String> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let mut hasher = Sha256::new();
    for entry in sorted {
        hasher.update(hex_decode(&entry.hash)?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hex_decode(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(Error::Cloud("invalid hash length".into()));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| Error::Cloud("invalid content hash".into()))
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn ensure_success(response: Response) -> Result<Response> {
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

fn parse_modified(value: Option<&str>) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(value?.parse::<i64>().ok()?)
        .single()
}

fn extract_tags(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| {
            value.as_str().map(str::to_owned).or_else(|| {
                value
                    .get("name")
                    .or_else(|| value.get("value"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        })
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn now_ms() -> String {
    Utc::now().timestamp_millis().to_string()
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".into()
    } else {
        format!("/{}", trimmed.trim_matches('/'))
    }
}

fn validate_name(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.len() > 255 || name.contains('/') || name.contains('\0') {
        return Err(Error::InvalidInput(
            "name must be 1-255 characters without '/'".into(),
        ));
    }
    Ok(())
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

async fn backoff(attempt: usize) {
    tokio::time::sleep(Duration::from_millis(
        150_u64.saturating_mul(1_u64 << attempt.min(5)),
    ))
    .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_index() {
        let entries =
            parse_index(b"3\naabb:0:doc.metadata:0:12\nccdd:0:doc.content:0:7\n").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "doc.metadata");
        assert_eq!(entries[1].size, 7);
    }

    #[test]
    fn root_serialization_is_stable() {
        let entries = vec![
            BlobEntry {
                hash: "bb".repeat(32),
                id: "b".into(),
                subfiles: 2,
                size: 20,
            },
            BlobEntry {
                hash: "aa".repeat(32),
                id: "a".into(),
                subfiles: 1,
                size: 10,
            },
        ];
        assert_eq!(
            String::from_utf8(serialize_root_index(&entries)).unwrap(),
            format!(
                "4\n0:.:2:30\n{}:0:a:1:10\n{}:0:b:2:20\n",
                "aa".repeat(32),
                "bb".repeat(32)
            )
        );
    }
}

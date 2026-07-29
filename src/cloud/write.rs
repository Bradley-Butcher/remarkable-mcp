use std::path::Path;

use chrono::Utc;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    CloudClient, MAX_ROOT_ATTEMPTS, ROOT_PUT_URL, ROOT_URL, RequestBody, RootResponse,
    index::{
        hash_entries, parse_index, serialize_document_index, serialize_root_index, sha256_hex,
    },
    transport::{backoff, ensure_success},
};
use crate::{
    error::{Error, Result},
    model::{BlobEntry, Item, MAX_UPLOAD_BYTES},
};

#[derive(Debug, Clone)]
enum MetadataChange {
    Rename(String),
    Move(String),
}

impl CloudClient {
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
}

fn now_ms() -> String {
    Utc::now().timestamp_millis().to_string()
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

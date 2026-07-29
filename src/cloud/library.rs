use chrono::{DateTime, TimeZone, Utc};
use futures_util::{StreamExt, stream};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;

use super::{CloudClient, ROOT_URL, RootResponse, index::parse_index};
use crate::{
    error::{Error, Result},
    model::{BlobEntry, Item, ItemKind, Library},
};

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

impl CloudClient {
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

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".into()
    } else {
        format!("/{}", trimmed.trim_matches('/'))
    }
}

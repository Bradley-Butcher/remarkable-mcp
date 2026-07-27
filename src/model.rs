use std::collections::HashMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DEFAULT_BROWSE_LIMIT: u8 = 25;
pub const MAX_BROWSE_LIMIT: u8 = 50;
pub const DEFAULT_SEARCH_LIMIT: u8 = 10;
pub const MAX_SEARCH_LIMIT: u8 = 20;
pub const DEFAULT_RECENT_LIMIT: u8 = 10;
pub const MAX_RECENT_LIMIT: u8 = 20;
pub const STANDARD_MAX_EDGE: u32 = 1_600;
pub const STANDARD_MAX_BYTES: usize = 1_048_576;
pub const HIGH_MAX_EDGE: u32 = 2_048;
pub const HIGH_MAX_BYTES: usize = 1_572_864;
pub const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Folder,
    Notebook,
    Pdf,
    Epub,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Notebook => "note",
            Self::Pdf => "pdf",
            Self::Epub => "epub",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlobEntry {
    pub hash: String,
    pub id: String,
    pub subfiles: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub hash: String,
    pub name: String,
    pub parent: String,
    pub kind: ItemKind,
    pub modified_at: Option<DateTime<Utc>>,
    pub size: u64,
    pub files: Vec<BlobEntry>,
    pub tags: Vec<String>,
}

impl Item {
    pub fn is_folder(&self) -> bool {
        self.kind == ItemKind::Folder
    }
}

#[derive(Debug, Clone, Default)]
pub struct Library {
    pub items: Vec<Item>,
    pub by_id: HashMap<String, usize>,
    pub paths: HashMap<String, usize>,
}

impl Library {
    pub fn new(items: Vec<Item>) -> Self {
        let by_id = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect::<HashMap<_, _>>();

        let mut library = Self {
            items,
            by_id,
            paths: HashMap::new(),
        };

        for index in 0..library.items.len() {
            let path = library.path_for(index);
            library.paths.insert(path.to_lowercase(), index);
        }
        library
    }

    pub fn path_for(&self, index: usize) -> String {
        let mut parts = vec![self.items[index].name.clone()];
        let mut parent = self.items[index].parent.as_str();
        let mut depth = 0;
        while !parent.is_empty() && parent != "trash" && depth < 128 {
            let Some(parent_index) = self.by_id.get(parent).copied() else {
                break;
            };
            parts.push(self.items[parent_index].name.clone());
            parent = self.items[parent_index].parent.as_str();
            depth += 1;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectInput {
    /// One-time code from the reMarkable connection page. Omit to open the page.
    pub code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowseInput {
    /// Folder path to browse.
    #[serde(default = "root_path")]
    pub path: String,
    /// Opaque cursor returned by an earlier browse call.
    pub cursor: Option<String>,
    /// Number of entries to return (default 25, maximum 50).
    #[serde(default = "browse_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u8,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    /// Text to match against document names, paths, and tags.
    #[schemars(length(min = 1, max = 256))]
    pub query: String,
    /// Folder path that scopes the search.
    #[serde(default = "root_path")]
    pub path: String,
    /// Opaque cursor returned by an earlier search call.
    pub cursor: Option<String>,
    /// Number of matches to return (default 10, maximum 20).
    #[serde(default = "search_limit")]
    #[schemars(range(min = 1, max = 20))]
    pub limit: u8,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecentInput {
    /// Number of documents to return (default 10, maximum 20).
    #[serde(default = "recent_limit")]
    #[schemars(range(min = 1, max = 20))]
    pub limit: u8,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    #[default]
    Standard,
    High,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Crop {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub x: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub y: f32,
    #[schemars(range(min = 0.000001, max = 1.0))]
    pub width: f32,
    #[schemars(range(min = 0.000001, max = 1.0))]
    pub height: f32,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    /// Document name or full library path.
    pub document: String,
    /// One-based physical page number.
    #[serde(default = "first_page")]
    #[schemars(range(min = 1))]
    pub page: u32,
    /// Optional normalized crop of the page.
    pub crop: Option<Crop>,
    /// Standard is normally sufficient; use high for a small crop or fine handwriting.
    #[serde(default)]
    pub detail: Detail,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadInput {
    /// Local PDF or EPUB path.
    pub file_path: String,
    /// Destination folder path.
    #[serde(default = "root_path")]
    pub parent_folder: String,
    /// Optional name shown in the reMarkable library.
    pub document_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MkdirInput {
    pub folder_name: String,
    #[serde(default = "root_path")]
    pub parent: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveInput {
    pub document: String,
    pub dest_folder: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameInput {
    pub document: String,
    pub new_name: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteInput {
    pub document: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadMetadata {
    pub document: String,
    pub page: u32,
    pub total_pages: u32,
}

#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub metadata: ReadMetadata,
}

fn root_path() -> String {
    "/".to_owned()
}

fn first_page() -> u32 {
    1
}

fn browse_limit() -> u8 {
    DEFAULT_BROWSE_LIMIT
}

fn search_limit() -> u8 {
    DEFAULT_SEARCH_LIMIT
}

fn recent_limit() -> u8 {
    DEFAULT_RECENT_LIMIT
}

use std::path::PathBuf;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::SecondsFormat;
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    tool, tool_handler, tool_router,
};

use crate::{
    cloud::CloudClient,
    error::{Error, Result as AppResult},
    model::{
        BrowseInput, ConnectInput, DeleteInput, MAX_BROWSE_LIMIT, MAX_RECENT_LIMIT,
        MAX_SEARCH_LIMIT, MkdirInput, MoveInput, ReadInput, RecentInput, RenameInput, SearchInput,
        UploadInput,
    },
    page_renderer::PageRenderer,
};

#[derive(Clone)]
pub struct RemarkableServer {
    cloud: CloudClient,
    renderer: PageRenderer,
}

impl RemarkableServer {
    pub fn new(cloud: CloudClient) -> Self {
        Self {
            renderer: PageRenderer::new(cloud.clone()),
            cloud,
        }
    }
}

#[tool_router]
impl RemarkableServer {
    #[tool(
        name = "remarkable_connect",
        description = "Connect this server to reMarkable Cloud. Call with no code to open the official connection page; call again with the one-time code to finish."
    )]
    async fn connect(&self, Parameters(input): Parameters<ConnectInput>) -> CallToolResult {
        match input
            .code
            .as_deref()
            .map(str::trim)
            .filter(|code| !code.is_empty())
        {
            None => {
                let opened = tokio::task::spawn_blocking(|| open::that(CloudClient::connect_url()))
                    .await
                    .is_ok_and(|result| result.is_ok());
                if opened {
                    success("Opened the reMarkable connection page. Paste the one-time code here.")
                } else {
                    success(format!(
                        "Open {} and paste the one-time code here.",
                        CloudClient::connect_url()
                    ))
                }
            }
            Some(code) => tool_result(
                async {
                    self.cloud.register(code).await?;
                    self.cloud.check_connection().await?;
                    Ok("Connected to reMarkable Cloud.".to_owned())
                }
                .await,
            ),
        }
    }

    #[tool(
        name = "remarkable_read",
        description = "Read exactly one reMarkable page as a bounded image. Returns one JPEG plus a short page label; never OCR or bulk page data."
    )]
    async fn read(&self, Parameters(input): Parameters<ReadInput>) -> CallToolResult {
        match self.renderer.read(input).await {
            Ok(page) => {
                let label = format!(
                    "{} · page {}/{}",
                    page.metadata.document, page.metadata.page, page.metadata.total_pages
                );
                CallToolResult::success(vec![
                    ContentBlock::image(STANDARD.encode(page.bytes), page.mime_type),
                    ContentBlock::text(label),
                ])
            }
            Err(error) => failure(error),
        }
    }

    #[tool(
        name = "remarkable_browse",
        description = "List one folder in reMarkable Cloud. Returns at most 50 concise path lines and an opaque cursor when more exist."
    )]
    async fn browse(&self, Parameters(input): Parameters<BrowseInput>) -> CallToolResult {
        tool_result(self.browse_text(input).await)
    }

    #[tool(
        name = "remarkable_search",
        description = "Search reMarkable names, paths, and tags. Returns at most 20 concise matches; no OCR and no document bodies."
    )]
    async fn search(&self, Parameters(input): Parameters<SearchInput>) -> CallToolResult {
        tool_result(self.search_text(input).await)
    }

    #[tool(
        name = "remarkable_recent",
        description = "List the most recently modified reMarkable documents. Returns at most 20 concise lines without previews."
    )]
    async fn recent(&self, Parameters(input): Parameters<RecentInput>) -> CallToolResult {
        tool_result(self.recent_text(input).await)
    }

    #[tool(
        name = "remarkable_status",
        description = "Check reMarkable Cloud connectivity and return one short status line."
    )]
    async fn status(&self) -> CallToolResult {
        tool_result(
            async {
                if !self.cloud.is_configured().await {
                    return Ok("Not connected. Call remarkable_connect.".to_owned());
                }
                self.cloud.check_connection().await?;
                let count = self.cloud.library(true).await?.items.len();
                Ok(format!("Connected · {count} library items."))
            }
            .await,
        )
    }

    #[tool(
        name = "remarkable_upload",
        description = "Upload one local PDF or EPUB to reMarkable Cloud. Returns one short confirmation."
    )]
    async fn upload(&self, Parameters(input): Parameters<UploadInput>) -> CallToolResult {
        tool_result(
            async {
                let parent_id = self.folder_id(&input.parent_folder).await?;
                let item = self
                    .cloud
                    .upload(
                        &PathBuf::from(&input.file_path),
                        &parent_id,
                        input.document_name.as_deref(),
                    )
                    .await?;
                Ok(format!("Uploaded {}.", item.name))
            }
            .await,
        )
    }

    #[tool(
        name = "remarkable_mkdir",
        description = "Create a folder in reMarkable Cloud. Returns one short confirmation."
    )]
    async fn mkdir(&self, Parameters(input): Parameters<MkdirInput>) -> CallToolResult {
        tool_result(
            async {
                let parent_id = self.folder_id(&input.parent).await?;
                let item = self
                    .cloud
                    .create_folder(&input.folder_name, &parent_id)
                    .await?;
                Ok(format!("Created folder {}.", item.name))
            }
            .await,
        )
    }

    #[tool(
        name = "remarkable_move",
        description = "Move a reMarkable document or folder. Returns one short confirmation."
    )]
    async fn move_item(&self, Parameters(input): Parameters<MoveInput>) -> CallToolResult {
        tool_result(
            async {
                let (library, index) = self.cloud.resolve(&input.document, false).await?;
                let item_id = library.items[index].id.clone();
                let parent_id = self.folder_id(&input.dest_folder).await?;
                ensure_not_descendant(&library, &item_id, &parent_id)?;
                let item = self.cloud.move_item(&item_id, &parent_id).await?;
                Ok(format!("Moved {} to {}.", item.name, input.dest_folder))
            }
            .await,
        )
    }

    #[tool(
        name = "remarkable_rename",
        description = "Rename a reMarkable document or folder. Returns one short confirmation."
    )]
    async fn rename(&self, Parameters(input): Parameters<RenameInput>) -> CallToolResult {
        tool_result(
            async {
                let (library, index) = self.cloud.resolve(&input.document, false).await?;
                let item = self
                    .cloud
                    .rename(&library.items[index].id, &input.new_name)
                    .await?;
                Ok(format!("Renamed to {}.", item.name))
            }
            .await,
        )
    }

    #[tool(
        name = "remarkable_delete",
        description = "Move a reMarkable document or folder to cloud trash. Returns one short confirmation."
    )]
    async fn delete(&self, Parameters(input): Parameters<DeleteInput>) -> CallToolResult {
        tool_result(
            async {
                let (library, index) = self.cloud.resolve(&input.document, false).await?;
                let name = library.items[index].name.clone();
                self.cloud.delete(&library.items[index].id).await?;
                Ok(format!("Moved {name} to trash."))
            }
            .await,
        )
    }
}

#[tool_handler(
    name = "remarkable-mcp",
    version = "0.1.0",
    instructions = "Focused reMarkable Cloud tools. Use remarkable_read for a single bounded page image; use browse/search/recent before mutations."
)]
impl ServerHandler for RemarkableServer {}

impl RemarkableServer {
    async fn folder_id(&self, path: &str) -> AppResult<String> {
        if normalize_path(path) == "/" {
            return Ok(String::new());
        }
        let (library, index) = self.cloud.resolve(path, true).await?;
        Ok(library.items[index].id.clone())
    }

    async fn browse_text(&self, input: BrowseInput) -> AppResult<String> {
        let path = normalize_path(&input.path);
        let parent_id = self.folder_id(&path).await?;
        let library = self.cloud.library(false).await?;
        let mut indices = library
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| item.parent == parent_id && is_visible(&library, *index))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by(|left, right| {
            library.items[*right]
                .is_folder()
                .cmp(&library.items[*left].is_folder())
                .then_with(|| {
                    library.items[*left]
                        .name
                        .to_lowercase()
                        .cmp(&library.items[*right].name.to_lowercase())
                })
        });
        paginate_lines(
            &library,
            &indices,
            input.cursor.as_deref(),
            input.limit.min(MAX_BROWSE_LIMIT),
        )
    }

    async fn search_text(&self, input: SearchInput) -> AppResult<String> {
        let query = input.query.trim().to_lowercase();
        if query.is_empty() {
            return Err(Error::InvalidInput("search query is empty".into()));
        }
        let scope = normalize_path(&input.path).to_lowercase();
        if scope != "/" {
            self.folder_id(&scope).await?;
        }
        let library = self.cloud.library(false).await?;
        let mut scored = library
            .items
            .iter()
            .enumerate()
            .filter(|(index, _)| is_visible(&library, *index))
            .filter_map(|(index, item)| {
                let path = library.path_for(index);
                if scope != "/" && !path.to_lowercase().starts_with(&(scope.clone() + "/")) {
                    return None;
                }
                let name = item.name.to_lowercase();
                let path_lower = path.to_lowercase();
                let tag_match = item
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&query));
                let score = if name == query {
                    0
                } else if name.starts_with(&query) {
                    1
                } else if name.contains(&query) {
                    2
                } else if path_lower.contains(&query) {
                    3
                } else if tag_match {
                    4
                } else {
                    return None;
                };
                Some((score, index))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| {
                library
                    .path_for(left.1)
                    .to_lowercase()
                    .cmp(&library.path_for(right.1).to_lowercase())
            })
        });
        let indices = scored
            .into_iter()
            .map(|(_, index)| index)
            .collect::<Vec<_>>();
        paginate_lines(
            &library,
            &indices,
            input.cursor.as_deref(),
            input.limit.min(MAX_SEARCH_LIMIT),
        )
    }

    async fn recent_text(&self, input: RecentInput) -> AppResult<String> {
        let library = self.cloud.library(false).await?;
        let mut indices = library
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| !item.is_folder() && is_visible(&library, *index))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by(|left, right| {
            library.items[*right]
                .modified_at
                .cmp(&library.items[*left].modified_at)
        });
        indices.truncate(input.limit.min(MAX_RECENT_LIMIT) as usize);
        if indices.is_empty() {
            return Ok("No documents.".into());
        }
        Ok(indices
            .into_iter()
            .map(|index| {
                let item = &library.items[index];
                let modified = item
                    .modified_at
                    .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
                    .unwrap_or_else(|| "unknown".into());
                format!(
                    "{modified}\t{}\t{}",
                    item.kind.label(),
                    library.path_for(index)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn paginate_lines(
    library: &crate::model::Library,
    indices: &[usize],
    cursor: Option<&str>,
    limit: u8,
) -> AppResult<String> {
    if limit == 0 {
        return Err(Error::InvalidInput("limit must be at least 1".into()));
    }
    let offset = decode_cursor(cursor)?;
    if offset > indices.len() {
        return Err(Error::InvalidInput("cursor is no longer valid".into()));
    }
    let end = (offset + limit as usize).min(indices.len());
    let mut lines = indices[offset..end]
        .iter()
        .map(|index| {
            let item = &library.items[*index];
            format!("{}\t{}", item.kind.label(), library.path_for(*index))
        })
        .collect::<Vec<_>>();
    if end < indices.len() {
        lines.push(format!("next_cursor: {}", encode_cursor(end)));
    }
    if lines.is_empty() {
        Ok("No items.".into())
    } else {
        Ok(lines.join("\n"))
    }
}

fn is_visible(library: &crate::model::Library, index: usize) -> bool {
    let mut parent = library.items[index].parent.as_str();
    let mut depth = 0;
    while !parent.is_empty() && depth < 128 {
        if parent == "trash" {
            return false;
        }
        let Some(parent_index) = library.by_id.get(parent).copied() else {
            break;
        };
        parent = library.items[parent_index].parent.as_str();
        depth += 1;
    }
    true
}

fn ensure_not_descendant(
    library: &crate::model::Library,
    item_id: &str,
    destination_id: &str,
) -> AppResult<()> {
    let mut current = destination_id;
    let mut depth = 0;
    while !current.is_empty() && depth < 128 {
        if current == item_id {
            return Err(Error::InvalidInput(
                "a folder cannot be moved into itself or its descendant".into(),
            ));
        }
        let Some(index) = library.by_id.get(current).copied() else {
            break;
        };
        current = library.items[index].parent.as_str();
        depth += 1;
    }
    Ok(())
}

fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "/" {
        "/".into()
    } else {
        format!("/{}", path.trim_matches('/'))
    }
}

fn encode_cursor(offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

fn decode_cursor(cursor: Option<&str>) -> AppResult<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| Error::InvalidInput("invalid cursor".into()))?;
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::InvalidInput("invalid cursor".into()))
}

fn success(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

fn failure(error: Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(error.to_string())])
}

fn tool_result(result: AppResult<String>) -> CallToolResult {
    match result {
        Ok(text) => success(text),
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_without_exposing_offsets() {
        let cursor = encode_cursor(37);
        assert_ne!(cursor, "37");
        assert_eq!(decode_cursor(Some(&cursor)).unwrap(), 37);
        assert!(decode_cursor(Some("not-valid")).is_err());
    }

    #[test]
    fn exposes_only_the_focused_tool_set() {
        let names = RemarkableServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "remarkable_browse",
                "remarkable_connect",
                "remarkable_delete",
                "remarkable_mkdir",
                "remarkable_move",
                "remarkable_read",
                "remarkable_recent",
                "remarkable_rename",
                "remarkable_search",
                "remarkable_status",
                "remarkable_upload",
            ]
        );
    }
}

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_sql, TrackResponse};

#[derive(Deserialize)]
pub struct FolderQuery {
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct FolderEntry {
    pub name: String,
    pub relative_path: String,
    pub is_dir: bool,
    pub track: Option<TrackResponse>,
}

#[derive(Serialize)]
pub struct FolderListing {
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<FolderEntry>,
}

pub async fn list_folder(
    claims: Claims,
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<FolderListing>, (StatusCode, String)> {
    let rel_path_str = query.path.as_deref().unwrap_or("").trim_matches(&['/', '\\'][..]);

    // Safety check: ensure target path stays inside data_dir
    let target_dir = if rel_path_str.is_empty() {
        state.config.data_dir.clone()
    } else {
        state.config.data_dir.join(rel_path_str)
    };

    let canonical_data_dir = std::fs::canonicalize(&state.config.data_dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Invalid data dir: {}", e)))?;

    let canonical_target = std::fs::canonicalize(&target_dir)
        .map_err(|_| (StatusCode::NOT_FOUND, "Folder not found".to_string()))?;

    if !canonical_target.starts_with(&canonical_data_dir) {
        return Err((StatusCode::FORBIDDEN, "Access denied outside library root".to_string()));
    }

    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let entries_dir = std::fs::read_dir(&canonical_target)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read directory: {}", e)))?;

    let audio_extensions = ["mp3", "flac", "m4a", "ogg", "wav", "alac", "aac"];

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry_res in entries_dir {
        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let file_name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files/folders
        if file_name.starts_with('.') {
            continue;
        }

        let full_entry_path = entry.path();
        let relative_to_data = match full_entry_path.strip_prefix(&canonical_data_dir) {
            Ok(p) => p.to_string_lossy().to_string().replace('\\', "/"),
            Err(_) => continue,
        };

        if file_type.is_dir() {
            dirs.push(FolderEntry {
                name: file_name,
                relative_path: relative_to_data,
                is_dir: true,
                track: None,
            });
        } else if file_type.is_file() {
            let ext = full_entry_path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            if audio_extensions.contains(&ext.as_str()) {
                // Try looking up track metadata in DB
                let track = sqlx::query_as::<_, TrackResponse>(
                    &track_select_sql("WHERE path = ?")
                )
                .bind(&relative_to_data)
                .fetch_optional(&user_pool)
                .await
                .ok()
                .flatten();

                files.push(FolderEntry {
                    name: file_name,
                    relative_path: relative_to_data,
                    is_dir: false,
                    track,
                });
            }
        }
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut all_entries = dirs;
    all_entries.extend(files);

    let parent_path = if rel_path_str.is_empty() {
        None
    } else {
        PathBuf::from(rel_path_str).parent().map(|p| p.to_string_lossy().to_string().replace('\\', "/"))
    };

    Ok(Json(FolderListing {
        current_path: rel_path_str.to_string(),
        parent_path,
        entries: all_entries,
    }))
}

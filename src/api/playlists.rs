use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_aliased_sql, TrackResponse};
use crate::events::ServerEvent;
use sqlx::Row;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct PlaylistResponse {
    pub id: i64,
    pub name: String,
    pub cover_url: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Deserialize)]
pub struct CreatePlaylistRequest {
    pub name: String,
    pub cover_url: Option<String>,
}

#[derive(Deserialize)]
pub struct AddTrackRequest {
    pub track_id: i64,
}

#[derive(Deserialize)]
pub struct ReorderPlaylistRequest {
    pub from_index: i64,
    pub to_index: i64,
}

async fn log_and_broadcast_event(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    event_type: &str,
    payload: serde_json::Value,
) {
    let payload_str = payload.to_string();
    if let Ok(res) = sqlx::query(
        "INSERT INTO events (event_type, payload) VALUES (?, ?)"
    )
    .bind(event_type)
    .bind(&payload_str)
    .execute(pool)
    .await
    {
        let event_id = res.last_insert_rowid();
        let time_now = chrono::Utc::now().to_rfc3339();
        state.event_bus.broadcast(ServerEvent {
            id: event_id,
            event_type: event_type.to_string(),
            payload,
            created_at: time_now,
        });
    }
}

pub async fn get_playlists(
    claims: Claims,
    State(state): State<AppState>,
) -> Result<Json<Vec<PlaylistResponse>>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let playlists = sqlx::query_as::<_, PlaylistResponse>(
        "SELECT id, name, cover_url, created_at FROM playlists WHERE user_id = ? ORDER BY created_at DESC"
    )
    .bind(&claims.sub)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(playlists))
}

pub async fn create_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Json(payload): Json<CreatePlaylistRequest>,
) -> Result<(StatusCode, Json<PlaylistResponse>), (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let res = sqlx::query(
        "INSERT INTO playlists (user_id, name, cover_url) VALUES (?, ?, ?)"
    )
    .bind(&claims.sub)
    .bind(&payload.name)
    .bind(&payload.cover_url)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let playlist_id = res.last_insert_rowid();

    let playlist = sqlx::query_as::<_, PlaylistResponse>(
        "SELECT id, name, cover_url, created_at FROM playlists WHERE id = ?"
    )
    .bind(playlist_id)
    .fetch_one(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.created",
        serde_json::to_value(&playlist).unwrap_or(serde_json::Value::Null)
    ).await;

    Ok((StatusCode::CREATED, Json(playlist)))
}

pub async fn get_playlist_by_id(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PlaylistResponse>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let playlist = sqlx::query_as::<_, PlaylistResponse>(
        "SELECT id, name, cover_url, created_at FROM playlists WHERE id = ? AND user_id = ?"
    )
    .bind(id)
    .bind(&claims.sub)
    .fetch_optional(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    Ok(Json(playlist))
}

pub async fn update_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<CreatePlaylistRequest>,
) -> Result<Json<PlaylistResponse>, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    sqlx::query(
        "UPDATE playlists SET name = ?, cover_url = ? WHERE id = ?"
    )
    .bind(&payload.name)
    .bind(&payload.cover_url)
    .bind(id)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let playlist = sqlx::query_as::<_, PlaylistResponse>(
        "SELECT id, name, cover_url, created_at FROM playlists WHERE id = ?"
    )
    .bind(id)
    .fetch_one(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.updated",
        serde_json::json!({ "id": id })
    ).await;

    Ok(Json(playlist))
}

pub async fn delete_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(id)
        .execute(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.deleted",
        serde_json::json!({ "id": id })
    ).await;

    Ok(StatusCode::OK)
}

pub async fn get_playlist_tracks(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<TrackResponse>>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership of the playlist
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    let tracks = sqlx::query_as::<_, TrackResponse>(
        &track_select_aliased_sql("JOIN playlist_tracks pt ON pt.track_id = t.id WHERE pt.playlist_id = ? ORDER BY pt.position, t.date_added")
    )
    .bind(id)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tracks))
}

pub async fn add_track_to_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<AddTrackRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    // Check if track is already in playlist
    let already_in = sqlx::query(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?"
    )
    .bind(id)
    .bind(payload.track_id)
    .fetch_optional(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if already_in.is_some() {
        return Ok(StatusCode::OK);
    }

    // Get max position
    let max_pos = sqlx::query("SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = ?")
        .bind(id)
        .fetch_one(&user_pool)
        .await
        .map(|r| r.get::<Option<i64>, _>(0).unwrap_or(-1))
        .unwrap_or(-1);

    let next_pos = max_pos + 1;

    sqlx::query(
        "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?, ?, ?)"
    )
    .bind(id)
    .bind(payload.track_id)
    .bind(next_pos)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.updated",
        serde_json::json!({ "id": id })
    ).await;

    Ok(StatusCode::CREATED)
}

pub async fn remove_track_from_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Path((playlist_id, track_id)): Path<(i64, i64)>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(playlist_id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    sqlx::query(
        "DELETE FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?"
    )
    .bind(playlist_id)
    .bind(track_id)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Shift positions down
    let tracks = sqlx::query(
        "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position"
    )
    .bind(playlist_id)
    .fetch_all(&user_pool)
    .await
    .unwrap_or_default();

    for (pos, tr) in tracks.into_iter().enumerate() {
        let new_pos = pos as i64;
        let track_id_val: i64 = tr.get("track_id");
        sqlx::query(
            "UPDATE playlist_tracks SET position = ? WHERE playlist_id = ? AND track_id = ?"
        )
        .bind(new_pos)
        .bind(playlist_id)
        .bind(track_id_val)
        .execute(&user_pool)
        .await
        .ok();
    }

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.updated",
        serde_json::json!({ "id": playlist_id })
    ).await;

    Ok(StatusCode::OK)
}

pub async fn reorder_playlist_tracks(
    claims: Claims,
    State(state): State<AppState>,
    Path(playlist_id): Path<i64>,
    Json(payload): Json<ReorderPlaylistRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(playlist_id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    let rows = sqlx::query(
        "SELECT track_id, position FROM playlist_tracks WHERE playlist_id = ? ORDER BY position"
    )
    .bind(playlist_id)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if rows.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Playlist is empty".to_string()));
    }

    let mut tracks = Vec::new();
    for row in rows {
        let track_id_val: i64 = row.get("track_id");
        let position_val: i64 = row.get("position");
        tracks.push((track_id_val, position_val));
    }

    let max_index = (tracks.len() - 1) as i64;
    let from_index = payload.from_index.clamp(0, max_index) as usize;
    let to_index = payload.to_index.clamp(0, max_index) as usize;

    if from_index == to_index {
        return Ok(StatusCode::OK);
    }

    let moved_item = tracks.remove(from_index);
    tracks.insert(to_index, moved_item);

    let mut transaction = user_pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for (new_pos, tr) in tracks.iter().enumerate() {
        let pos_val = new_pos as i64;
        sqlx::query(
            "UPDATE playlist_tracks SET position = ? WHERE playlist_id = ? AND track_id = ?"
        )
        .bind(pos_val)
        .bind(playlist_id)
        .bind(tr.0)
        .execute(&mut *transaction)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    transaction.commit().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    log_and_broadcast_event(
        &state,
        &user_pool,
        "playlist.updated",
        serde_json::json!({ "id": playlist_id, "fromIndex": payload.from_index, "toIndex": payload.to_index })
    ).await;

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct BulkAddTracksRequest {
    pub track_ids: Vec<i64>,
}

pub async fn bulk_add_tracks_to_playlist(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<BulkAddTracksRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 1. Verify ownership of the playlist
    let _existing = sqlx::query("SELECT id FROM playlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(&claims.sub)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Playlist not found".to_string()))?;

    // 2. Start a transaction
    let mut tx = user_pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 3. Get current max position
    let max_pos = sqlx::query("SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map(|r| r.get::<Option<i64>, _>(0).unwrap_or(-1))
        .unwrap_or(-1);

    let mut next_pos = max_pos + 1;
    let mut added_any = false;
    let total = payload.track_ids.len();

    // 4. Loop over all requested tracks
    for (idx, track_id) in payload.track_ids.iter().copied().enumerate() {
        // Check if track is already in playlist
        let already_in = sqlx::query(
            "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?"
        )
        .bind(id)
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if already_in.is_some() {
            let progress_payload = serde_json::json!({
                "action": "playlist",
                "current": idx + 1,
                "total": total,
                "track_id": track_id,
            });
            log_and_broadcast_event(&state, &user_pool, "bulk.progress", progress_payload).await;
            continue;
        }

        // Insert
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?, ?, ?)"
        )
        .bind(id)
        .bind(track_id)
        .bind(next_pos)
        .execute(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        next_pos += 1;
        added_any = true;

        let progress_payload = serde_json::json!({
            "action": "playlist",
            "current": idx + 1,
            "total": total,
            "track_id": track_id,
        });
        log_and_broadcast_event(&state, &user_pool, "bulk.progress", progress_payload).await;
    }

    // 5. Commit transaction
    tx.commit().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if added_any {
        log_and_broadcast_event(
            &state,
            &user_pool,
            "playlist.updated",
            serde_json::json!({ "id": id })
        ).await;
    }

    let completed_payload = serde_json::json!({
        "action": "playlist",
        "total": total,
    });
    log_and_broadcast_event(&state, &user_pool, "bulk.completed", completed_payload).await;

    Ok(StatusCode::OK)
}


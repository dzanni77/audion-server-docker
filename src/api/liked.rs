use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_aliased_sql, TrackResponse};
use crate::events::ServerEvent;

pub async fn get_liked_tracks(
    claims: Claims,
    State(state): State<AppState>,
) -> Result<Json<Vec<TrackResponse>>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let tracks = sqlx::query_as::<_, TrackResponse>(
        &track_select_aliased_sql("JOIN liked_tracks lt ON lt.track_id = t.id WHERE lt.user_id = ? ORDER BY lt.liked_at DESC")
    )
    .bind(&claims.sub)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tracks))
}

pub async fn like_track(
    claims: Claims,
    State(state): State<AppState>,
    Path(track_id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify track exists
    let _exists = sqlx::query("SELECT id FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Track not found".to_string()))?;

    sqlx::query(
        "INSERT OR IGNORE INTO liked_tracks (user_id, track_id) VALUES (?, ?)"
    )
    .bind(&claims.sub)
    .bind(track_id)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Record and broadcast event
    let payload = serde_json::json!({ "track_id": track_id, "liked": true });
    let payload_str = payload.to_string();
    let event_type = "liked.changed";

    if let Ok(res) = sqlx::query(
        "INSERT INTO events (event_type, payload) VALUES (?, ?)"
    )
    .bind(event_type)
    .bind(&payload_str)
    .execute(&user_pool)
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

    Ok(StatusCode::OK)
}

pub async fn unlike_track(
    claims: Claims,
    State(state): State<AppState>,
    Path(track_id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        "DELETE FROM liked_tracks WHERE user_id = ? AND track_id = ?"
    )
    .bind(&claims.sub)
    .bind(track_id)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Record and broadcast event
    let payload = serde_json::json!({ "track_id": track_id, "liked": false });
    let payload_str = payload.to_string();
    let event_type = "liked.changed";

    if let Ok(res) = sqlx::query(
        "INSERT INTO events (event_type, payload) VALUES (?, ?)"
    )
    .bind(event_type)
    .bind(&payload_str)
    .execute(&user_pool)
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

    Ok(StatusCode::OK)
}

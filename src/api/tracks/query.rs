use super::*;

pub async fn get_tracks(
    _claims: Claims,
    State(state): State<AppState>,
    Query(query): Query<PaginatedQuery>,
) -> Result<Json<Vec<TrackResponse>>, (StatusCode, String)> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(50).max(1);
    let offset = (page - 1) * limit;

    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let tracks = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("ORDER BY artist, album, disc_number, track_number, title LIMIT ? OFFSET ?")
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tracks))
}

pub async fn get_track_by_id(
    _claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<TrackResponse>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let track = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE id = ?")
    )
    .bind(id)
    .fetch_optional(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((StatusCode::NOT_FOUND, "Track not found".to_string()))?;

    Ok(Json(track))
}

pub async fn delete_track_inner(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    id: i64,
) -> Result<(), (StatusCode, String)> {
    let track = sqlx::query("SELECT path, track_cover_path FROM tracks WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Track not found".to_string()))?;

    let path_val = track.get::<String, _>("path");
    let cover_path_val = track.get::<Option<String>, _>("track_cover_path");

    // Delete files
    {
        let storage = state.storage_backend.read().await;
        let _ = storage.delete_object(&path_val).await;
        if let Some(ref cover_path) = cover_path_val {
            let _ = storage.delete_object(cover_path).await;
        }
    }
    let transcoded_path = state.config.data_dir.join("transcoded").join(format!("{}.mp3", id));
    if transcoded_path.exists() {
        let _ = std::fs::remove_file(transcoded_path);
    }

    // Delete from DB
    sqlx::query("DELETE FROM tracks WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Record Event in DB
    let payload = serde_json::json!({ "id": id });
    let payload_str = payload.to_string();
    let event_type = "track.deleted";

    let event_res = sqlx::query("INSERT INTO events (event_type, payload) VALUES (?, ?)")
        .bind(event_type)
        .bind(payload_str)
        .execute(pool)
        .await;

    if let Ok(er) = event_res {
        let event_id = er.last_insert_rowid();
        let time_now = chrono::Utc::now().to_rfc3339();

        // Broadcast Event
        state.event_bus.broadcast(ServerEvent {
            id: event_id,
            event_type: event_type.to_string(),
            payload,
            created_at: time_now,
        });
    }

    info!("Successfully deleted track id: {}", id);
    Ok(())
}

pub async fn delete_track(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    delete_track_inner(&state, &user_pool, id).await?;
    Ok(StatusCode::OK)
}

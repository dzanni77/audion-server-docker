use super::*;

pub async fn upload_track(
    claims: Claims,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<TrackResponse>), (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut file_bytes = Vec::new();
    let mut original_filename = String::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            original_filename = field.file_name().unwrap_or("upload.mp3").to_string();
            file_bytes = field.bytes().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?.to_vec();
        }
    }

    if file_bytes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing file payload".to_string()));
    }

    // Check if user is allowed to upload and check their storage quota
    let (can_upload, storage_quota_bytes): (i32, Option<i64>) = sqlx::query_as(
        "SELECT can_upload, storage_quota_bytes FROM users WHERE id = ?"
    )
    .bind(&claims.sub)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to retrieve user upload settings: {}", e)))?;

    if can_upload == 0 {
        return Err((StatusCode::FORBIDDEN, "Upload access has been disabled for your account".to_string()));
    }

    if let Some(quota) = storage_quota_bytes {
        let current_usage: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM tracks")
            .fetch_one(&user_pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to query storage usage: {}", e)))?;

        let new_file_size = file_bytes.len() as i64;
        if current_usage + new_file_size > quota {
            return Err((StatusCode::INSUFFICIENT_STORAGE, format!(
                "Storage quota exceeded. Limit is {} bytes, but current usage is {} bytes and uploading this file would exceed that.",
                quota, current_usage
            )));
        }
    }

    // Generate unique storage name
    let file_uuid = Uuid::new_v4().to_string();
    let ext = PathBuf::from(&original_filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("mp3")
        .to_string();
    let relative_path = format!("users/{}/music/{}.{}", claims.sub, file_uuid, ext);
    let full_path = state.config.data_dir.join(&relative_path);

    // Ensure music directory exists
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Write file temporarily to run metadata extraction
    std::fs::write(&full_path, &file_bytes).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Extract tags
    let mut metadata = extract_metadata(&full_path.to_string_lossy())
        .ok_or_else(|| {
            let _ = std::fs::remove_file(&full_path);
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Failed to extract metadata".to_string())
        })?;

    // If the extracted title matches the generated UUID, it fell back to the disk filename.
    // In this case, override the title using the original filename's stem.
    if let Some(ref title) = metadata.title {
        if title == &file_uuid {
            let orig_path = std::path::Path::new(&original_filename);
            metadata.title = orig_path.file_stem().map(|s| s.to_string_lossy().to_string());
        }
    }

    // Deduplicate by content hash
    if let Some(ref hash) = metadata.content_hash {
        let existing = sqlx::query("SELECT id FROM tracks WHERE content_hash = ?")
            .bind(hash)
            .fetch_optional(&user_pool)
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&full_path);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;

        if existing.is_some() {
            let _ = std::fs::remove_file(&full_path);
            return Err((StatusCode::CONFLICT, "Duplicate track detected".to_string()));
        }
    }

    // If S3 is active, upload track and delete local temp file
    let upload_to_s3 = {
        let storage = state.storage_backend.read().await;
        if let crate::storage::StorageBackend::S3 { .. } = &*storage {
            let content_type = match ext.to_lowercase().as_str() {
                "mp3" => "audio/mpeg",
                "flac" => "audio/flac",
                "m4a" | "aac" | "alac" => "audio/mp4",
                "ogg" => "audio/ogg",
                "wav" => "audio/wav",
                _ => "application/octet-stream",
            };
            storage.put_object(&relative_path, file_bytes.clone(), content_type).await
                .map_err(|e| {
                    let _ = std::fs::remove_file(&full_path);
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to upload track to S3: {}", e))
                })?;
            true
        } else {
            false
        }
    };
    if upload_to_s3 {
        let _ = std::fs::remove_file(&full_path);
    }

    // Get or Create Album
    let album_id = if let Some(album_name) = &metadata.album {
        let existing_album = sqlx::query("SELECT id FROM albums WHERE name = ?")
            .bind(album_name)
            .fetch_optional(&user_pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if let Some(alb) = existing_album {
            Some(alb.get::<i64, _>("id"))
        } else {
            let res = sqlx::query("INSERT INTO albums (name, artist) VALUES (?, ?)")
                .bind(album_name)
                .bind(&metadata.artist)
                .execute(&user_pool)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Some(res.last_insert_rowid())
        }
    } else {
        None
    };

    // Insert track into DB
    let res = sqlx::query(
        "INSERT INTO tracks (
            path, title, artist, album, track_number, disc_number, duration,
            album_id, format, bitrate, source_type, cover_url, external_id,
            local_src, genre, metadata_json, size
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'server', ?, ?, ?, ?, ?, ?)"
    )
    .bind(&relative_path)
    .bind(&metadata.title)
    .bind(&metadata.artist)
    .bind(&metadata.album)
    .bind(metadata.track_number)
    .bind(metadata.disc_number)
    .bind(metadata.duration)
    .bind(album_id)
    .bind(&metadata.format)
    .bind(metadata.bitrate)
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(&metadata.genre)
    .bind(&metadata.metadata_json)
    .bind(file_bytes.len() as i64)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let track_id = res.last_insert_rowid();

    // Ensure artwork directory exists
    let artwork_dir = state.config.user_artwork_dir(&claims.sub);
    std::fs::create_dir_all(&artwork_dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Save track cover if present
    if let Some(cover_data) = &metadata.track_cover {
        let relative_cover = format!("users/{}/artwork/track_{}.jpg", claims.sub, track_id);
        let write_success = {
            let storage = state.storage_backend.read().await;
            storage.put_object(&relative_cover, cover_data.clone(), "image/jpeg").await.is_ok()
        };
        if write_success {
            sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                .bind(&relative_cover)
                .bind(track_id)
                .execute(&user_pool)
                .await
                .ok();
        }
    }

    // Save album art if present and album doesn't have art yet
    if let Some(alb_id) = album_id {
        if let Some(art_data) = &metadata.album_art {
            let has_art = sqlx::query("SELECT art_path FROM albums WHERE id = ? AND art_path IS NOT NULL")
                .bind(alb_id)
                .fetch_optional(&user_pool)
                .await
                .map(|o| o.is_some())
                .unwrap_or(false);

            if !has_art {
                let relative_art = format!("users/{}/artwork/album_{}.jpg", claims.sub, alb_id);
                let write_success = {
                    let storage = state.storage_backend.read().await;
                    storage.put_object(&relative_art, art_data.clone(), "image/jpeg").await.is_ok()
                };
                if write_success {
                    sqlx::query("UPDATE albums SET art_path = ? WHERE id = ?")
                        .bind(&relative_art)
                        .bind(alb_id)
                        .execute(&user_pool)
                        .await
                        .ok();
                }
            }
        }
    }

    // Retrieve full newly created track
    let track = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE id = ?")
    )
    .bind(track_id)
    .fetch_one(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Record Event in DB
    let payload = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
    let payload_str = payload.to_string();
    let event_type = "track.added";

    let event_res = sqlx::query("INSERT INTO events (event_type, payload) VALUES (?, ?)")
        .bind(event_type)
        .bind(payload_str)
        .execute(&user_pool)
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

    info!("Successfully uploaded track: {}", track.title.as_deref().unwrap_or(""));
    Ok((StatusCode::CREATED, Json(track)))
}

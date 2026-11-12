use super::*;

pub async fn get_scan_status(
    claims: Claims,
    State(state): State<AppState>,
) -> Json<LibraryStatusResponse> {
    let status_lock = state.get_user_scan_status(&claims.sub).await;
    let status = status_lock.lock().unwrap();
    Json(LibraryStatusResponse {
        is_scanning: status.is_scanning,
        files_scanned: status.files_scanned,
        total_files: status.total_files,
        current_file: status.current_file.clone(),
    })
}

pub async fn trigger_auto_scan(state: AppState, user_id: String) -> bool {
    let scan_status = state.get_user_scan_status(&user_id).await;
    {
        let mut status = scan_status.lock().unwrap();
        if status.is_scanning {
            return false;
        }

        status.is_scanning = true;
        status.files_scanned = 0;
        status.total_files = 0;
        status.current_file = None;
    }

    let pool = match state.get_user_pool(&user_id).await {
        Ok(p) => p,
        Err(_) => return false,
    };
    let config = state.config.clone();
    let event_bus = state.event_bus.clone();
    let scan_status_clone = scan_status.clone();
    let user_id_clone = user_id.clone();

    tokio::spawn(async move {
        info!("Starting background music directory scan for user {}...", user_id_clone);
        let music_dir = config.user_music_dir(&user_id_clone);
        
        // Ensure user music directory exists
        std::fs::create_dir_all(&music_dir).ok();

        let mut audio_files = Vec::new();
        collect_audio_files(&music_dir, &mut audio_files);
        
        {
            let mut status = scan_status_clone.lock().unwrap();
            status.total_files = audio_files.len();
        }

        broadcast_library_event(&event_bus, "scan.started", serde_json::json!({
            "total_files": audio_files.len()
        }));

        for (index, file_path) in audio_files.iter().enumerate() {
            let relative_path_res = file_path.strip_prefix(&config.data_dir);
            if relative_path_res.is_err() {
                continue;
            }
            let relative_path = relative_path_res.unwrap().to_string_lossy().to_string();

            // Update scan status
            {
                let mut status = scan_status_clone.lock().unwrap();
                status.files_scanned = index + 1;
                status.current_file = Some(file_path.file_name().unwrap_or_default().to_string_lossy().to_string());
            }

            // Periodically broadcast progress
            if (index + 1) % 5 == 0 || index + 1 == audio_files.len() {
                let status_clone = {
                    let status = scan_status_clone.lock().unwrap();
                    status.clone()
                };
                broadcast_library_event(&event_bus, "scan.progress", serde_json::to_value(&status_clone).unwrap_or(serde_json::Value::Null));
            }

            // Check if track is already in database
            let path_exists = sqlx::query("SELECT id FROM tracks WHERE path = ?")
                .bind(&relative_path)
                .fetch_optional(&pool)
                .await
                .map(|o| o.is_some())
                .unwrap_or(false);

            if path_exists {
                continue;
            }

            if let Some(mut metadata) = extract_metadata(&file_path.to_string_lossy()) {
                let mut is_duplicate = false;
                if let Some(ref hash) = metadata.content_hash {
                    let hash_exists = sqlx::query("SELECT id FROM tracks WHERE content_hash = ?")
                        .bind(hash)
                        .fetch_optional(&pool)
                        .await
                        .map(|o| o.is_some())
                        .unwrap_or(false);
                    if hash_exists {
                        is_duplicate = true;
                    }
                }

                if is_duplicate {
                    continue;
                }

                let album_id = if let Some(album_name) = &metadata.album {
                    let existing_album = sqlx::query("SELECT id FROM albums WHERE name = ?")
                        .bind(album_name)
                        .fetch_optional(&pool)
                        .await
                        .unwrap_or(None);

                    if let Some(alb) = existing_album {
                        Some(alb.get::<i64, _>("id"))
                    } else {
                        let res = sqlx::query("INSERT INTO albums (name, artist) VALUES (?, ?)")
                            .bind(album_name)
                            .bind(&metadata.artist)
                            .execute(&pool)
                            .await;
                        
                        match res {
                            Ok(r) => Some(r.last_insert_rowid()),
                            Err(_) => None,
                        }
                    }
                } else {
                    None
                };

                let file_size = std::fs::metadata(file_path).map(|m| m.len() as i64).unwrap_or(0);

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
                .bind(file_size)
                .execute(&pool)
                .await;

                if let Ok(r) = res {
                    let track_id = r.last_insert_rowid();
                    let artwork_dir = config.user_artwork_dir(&user_id_clone);
                    std::fs::create_dir_all(&artwork_dir).ok();

                    if let Some(cover_data) = &metadata.track_cover {
                        let relative_cover = format!("users/{}/artwork/track_{}.jpg", user_id_clone, track_id);
                        let cover_full = config.data_dir.join(&relative_cover);
                        if std::fs::write(&cover_full, cover_data).is_ok() {
                            sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                                .bind(&relative_cover)
                                .bind(track_id)
                                .execute(&pool)
                                .await
                                .ok();
                        }
                    }

                    if let Some(alb_id) = album_id {
                        if let Some(art_data) = &metadata.album_art {
                            let has_art = sqlx::query("SELECT art_path FROM albums WHERE id = ? AND art_path IS NOT NULL")
                                .bind(alb_id)
                                .fetch_optional(&pool)
                                .await
                                .map(|o| o.is_some())
                                .unwrap_or(false);

                            if !has_art {
                                let relative_art = format!("users/{}/artwork/album_{}.jpg", user_id_clone, alb_id);
                                let art_full = config.data_dir.join(&relative_art);
                                if std::fs::write(&art_full, art_data).is_ok() {
                                    sqlx::query("UPDATE albums SET art_path = ? WHERE id = ?")
                                        .bind(&relative_art)
                                        .bind(alb_id)
                                        .execute(&pool)
                                        .await
                                        .ok();
                                }
                            }
                        }
                    }

                    if let Ok(track) = sqlx::query_as::<_, TrackResponse>(
                        &track_select_sql("WHERE id = ?")
                    )
                    .bind(track_id)
                    .fetch_one(&pool)
                    .await {
                        let payload = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
                        broadcast_library_event(&event_bus, "track.added", payload);
                    }
                }
            }
        }

        {
            let mut status = scan_status_clone.lock().unwrap();
            status.is_scanning = false;
        }

        let final_status = {
            let status = scan_status_clone.lock().unwrap();
            status.clone()
        };

        broadcast_library_event(&event_bus, "scan.completed", serde_json::to_value(&final_status).unwrap_or(serde_json::Value::Null));
        info!("Background music directory scan for user {} completed.", user_id_clone);
    });

    true
}

pub async fn start_scan(
    claims: Claims,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    if trigger_auto_scan(state, claims.sub).await {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err((StatusCode::CONFLICT, "Scan already in progress".to_string()))
    }
}

pub fn collect_audio_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_audio_files(&path, files);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    match ext.to_lowercase().as_str() {
                        "mp3" | "flac" | "m4a" | "m4b" | "ogg" | "wav" | "alac" | "aac" => {
                            files.push(path);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

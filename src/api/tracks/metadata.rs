use super::*;

#[derive(Deserialize)]
pub struct UpdateMetadataRequest {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub comment: Option<String>,
    pub bpm: Option<i32>,
    pub isrc: Option<String>,
    pub lyrics: Option<String>,
}

#[derive(Deserialize)]
pub struct BulkMetadataUpdateRequest {
    pub track_ids: Vec<i64>,
    pub metadata: UpdateMetadataRequest,
}

pub async fn bulk_update_metadata(
    claims: Claims,
    State(state): State<AppState>,
    Json(payload): Json<BulkMetadataUpdateRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total = payload.track_ids.len();
    for (idx, id) in payload.track_ids.iter().copied().enumerate() {
        let req = UpdateMetadataRequest {
            title: payload.metadata.title.clone(),
            artist: payload.metadata.artist.clone(),
            album: payload.metadata.album.clone(),
            album_artist: payload.metadata.album_artist.clone(),
            composer: payload.metadata.composer.clone(),
            year: payload.metadata.year.clone(),
            genre: payload.metadata.genre.clone(),
            track_number: payload.metadata.track_number,
            disc_number: payload.metadata.disc_number,
            comment: payload.metadata.comment.clone(),
            bpm: payload.metadata.bpm,
            isrc: payload.metadata.isrc.clone(),
            lyrics: payload.metadata.lyrics.clone(),
        };

        let _ = update_track_metadata(claims.clone(), State(state.clone()), Path(id), Json(req)).await;

        let progress_payload = serde_json::json!({
            "action": "bulk_update",
            "current": idx + 1,
            "total": total,
            "track_id": id,
        });
        broadcast_bulk_event(&state, &user_pool, "bulk.progress", progress_payload).await;
    }

    let completed_payload = serde_json::json!({
        "action": "bulk_update",
        "total": total,
    });
    broadcast_bulk_event(&state, &user_pool, "bulk.completed", completed_payload).await;

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct SingleFetchRequest {
    pub provider: String,
}

#[derive(Deserialize)]
pub struct BulkFetchRequest {
    pub track_ids: Vec<i64>,
    pub provider: String,
}

#[derive(Deserialize)]
pub struct BulkDeleteRequest {
    pub track_ids: Vec<i64>,
}

pub async fn update_track_metadata_inner(
    state: &AppState,
    claims: &Claims,
    id: i64,
    payload: UpdateMetadataRequest,
) -> Result<TrackResponse, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Fetch current track details
    let current_track = sqlx::query("SELECT id, album_id, album, artist, path, metadata_json FROM tracks WHERE id = ?")
        .bind(id)
        .fetch_optional(&user_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Track not found".to_string()))?;

    let old_album_id: Option<i64> = current_track.get("album_id");
    let old_album_name: Option<String> = current_track.get("album");
    let old_artist: Option<String> = current_track.get("artist");
    let relative_path: String = current_track.get("path");
    let old_metadata_json: Option<String> = current_track.get("metadata_json");

    // Handle Album ID updates if album name changes
    let mut new_album_id = old_album_id;
    if let Some(ref new_album_name) = payload.album {
        let name_changed = Some(new_album_name) != old_album_name.as_ref();
        if name_changed {
            if new_album_name.trim().is_empty() {
                new_album_id = None;
            } else {
                let existing = sqlx::query("SELECT id FROM albums WHERE name = ?")
                    .bind(new_album_name)
                    .fetch_optional(&user_pool)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

                if let Some(alb) = existing {
                    new_album_id = Some(alb.get::<i64, _>("id"));
                } else {
                    let res = sqlx::query("INSERT INTO albums (name, artist) VALUES (?, ?)")
                        .bind(new_album_name)
                        .bind(&payload.artist.as_ref().or(old_artist.as_ref()))
                        .execute(&user_pool)
                        .await
                        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                    new_album_id = Some(res.last_insert_rowid());
                }
            }
        }
    }

    // Merge changes into metadata_json
    let mut metadata_map = old_metadata_json
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
        .unwrap_or_default();

    if let Some(ref t) = payload.title {
        metadata_map.insert("TrackTitle".to_string(), serde_json::Value::String(t.clone()));
    }
    if let Some(ref a) = payload.artist {
        metadata_map.insert("TrackArtist".to_string(), serde_json::Value::String(a.clone()));
    }
    if let Some(ref al) = payload.album {
        metadata_map.insert("AlbumTitle".to_string(), serde_json::Value::String(al.clone()));
    }
    if let Some(ref aa) = payload.album_artist {
        metadata_map.insert("AlbumArtist".to_string(), serde_json::Value::String(aa.clone()));
    }
    if let Some(ref c) = payload.composer {
        metadata_map.insert("Composer".to_string(), serde_json::Value::String(c.clone()));
    }
    if let Some(ref y) = payload.year {
        metadata_map.insert("Year".to_string(), serde_json::Value::String(y.clone()));
    }
    if let Some(ref g) = payload.genre {
        metadata_map.insert("Genre".to_string(), serde_json::Value::String(g.clone()));
    }
    if let Some(tr) = payload.track_number {
        metadata_map.insert("TrackNumber".to_string(), serde_json::Value::String(tr.to_string()));
    }
    if let Some(ds) = payload.disc_number {
        metadata_map.insert("DiscNumber".to_string(), serde_json::Value::String(ds.to_string()));
    }
    if let Some(ref cm) = payload.comment {
        metadata_map.insert("Comment".to_string(), serde_json::Value::String(cm.clone()));
    }
    if let Some(bpm) = payload.bpm {
        metadata_map.insert("Bpm".to_string(), serde_json::Value::String(bpm.to_string()));
    }
    if let Some(ref i) = payload.isrc {
        metadata_map.insert("Isrc".to_string(), serde_json::Value::String(i.clone()));
    }
    if let Some(ref l) = payload.lyrics {
        metadata_map.insert("Lyrics".to_string(), serde_json::Value::String(l.clone()));
    }

    let new_metadata_json = if metadata_map.is_empty() {
        None
    } else {
        serde_json::to_string(&metadata_map).ok()
    };

    // Update database row
    sqlx::query(
        "UPDATE tracks
         SET title = ?, artist = ?, album = ?, album_id = ?, genre = ?, track_number = ?, disc_number = ?,
             album_artist = ?, composer = ?, year = ?, comment = ?, bpm = ?, isrc = ?, lyrics = ?, metadata_json = ?
         WHERE id = ?"
    )
    .bind(&payload.title)
    .bind(&payload.artist)
    .bind(&payload.album)
    .bind(new_album_id)
    .bind(&payload.genre)
    .bind(payload.track_number)
    .bind(payload.disc_number)
    .bind(&payload.album_artist)
    .bind(&payload.composer)
    .bind(&payload.year)
    .bind(&payload.comment)
    .bind(payload.bpm)
    .bind(&payload.isrc)
    .bind(&payload.lyrics)
    .bind(new_metadata_json)
    .bind(id)
    .execute(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Clean up old empty albums
    if let Some(o_alb_id) = old_album_id {
        if Some(o_alb_id) != new_album_id {
            let tracks_left: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE album_id = ?")
                .bind(o_alb_id)
                .fetch_one(&user_pool)
                .await
                .unwrap_or(0);

            if tracks_left == 0 {
                let art_path_opt: Option<String> = sqlx::query_scalar("SELECT art_path FROM albums WHERE id = ?")
                    .bind(o_alb_id)
                    .fetch_one(&user_pool)
                    .await
                    .unwrap_or(None);

                if let Some(art_path) = art_path_opt {
                    let deleted = {
                        let storage = state.storage_backend.read().await;
                        match &*storage {
                            crate::storage::StorageBackend::Local { .. } => {
                                let full_art_path = state.config.data_dir.join(&art_path);
                                if full_art_path.exists() {
                                    let _ = std::fs::remove_file(full_art_path);
                                }
                                true
                            }
                            crate::storage::StorageBackend::S3 { .. }
                            | crate::storage::StorageBackend::Azure { .. } => {
                                let _ = storage.delete_object(&art_path).await;
                                true
                            }
                        }
                    };
                    let _ = deleted;
                }

                sqlx::query("DELETE FROM albums WHERE id = ?")
                    .bind(o_alb_id)
                    .execute(&user_pool)
                    .await
                    .ok();
            }
        }
    }

    // Try writing tags to physical file using lofty
    let full_path = state.config.data_dir.join(&relative_path);
    let payload_title = payload.title.clone();
    let payload_artist = payload.artist.clone();
    let payload_album = payload.album.clone();
    let payload_album_artist = payload.album_artist.clone();
    let payload_composer = payload.composer.clone();
    let payload_year = payload.year.clone();
    let payload_genre = payload.genre.clone();
    let payload_track_number = payload.track_number;
    let payload_disc_number = payload.disc_number;
    let payload_comment = payload.comment.clone();
    let payload_bpm = payload.bpm;
    let payload_isrc = payload.isrc.clone();
    let payload_lyrics = payload.lyrics.clone();

    tokio::task::spawn_blocking(move || {
        if full_path.exists() {
            use lofty::prelude::*;
            if let Ok(mut tagged_file) = lofty::probe::Probe::open(&full_path)
                .and_then(|p| p.options(lofty::config::ParseOptions::new().parsing_mode(lofty::config::ParsingMode::Relaxed)).read())
            {
                let tag = if tagged_file.primary_tag().is_some() {
                    tagged_file.primary_tag_mut()
                } else {
                    tagged_file.first_tag_mut()
                };
                if let Some(tag) = tag {
                    if let Some(t) = payload_title {
                        tag.insert_text(lofty::tag::ItemKey::TrackTitle, t);
                    }
                    if let Some(a) = payload_artist {
                        tag.insert_text(lofty::tag::ItemKey::TrackArtist, a);
                    }
                    if let Some(al) = payload_album {
                        tag.insert_text(lofty::tag::ItemKey::AlbumTitle, al);
                    }
                    if let Some(aa) = payload_album_artist {
                        tag.insert_text(lofty::tag::ItemKey::AlbumArtist, aa);
                    }
                    if let Some(c) = payload_composer {
                        tag.insert_text(lofty::tag::ItemKey::Composer, c);
                    }
                    if let Some(y) = payload_year {
                        tag.insert_text(lofty::tag::ItemKey::Year, y);
                    }
                    if let Some(g) = payload_genre {
                        tag.insert_text(lofty::tag::ItemKey::Genre, g);
                    }
                    if let Some(tr) = payload_track_number {
                        tag.insert_text(lofty::tag::ItemKey::TrackNumber, tr.to_string());
                    }
                    if let Some(ds) = payload_disc_number {
                        tag.insert_text(lofty::tag::ItemKey::DiscNumber, ds.to_string());
                    }
                    if let Some(cm) = payload_comment {
                        tag.insert_text(lofty::tag::ItemKey::Comment, cm);
                    }
                    if let Some(b) = payload_bpm {
                        tag.insert_text(lofty::tag::ItemKey::Bpm, b.to_string());
                    }
                    if let Some(i) = payload_isrc {
                        tag.insert_text(lofty::tag::ItemKey::Isrc, i);
                    }
                    if let Some(l) = payload_lyrics {
                        tag.insert_text(lofty::tag::ItemKey::Lyrics, l);
                    }
                    let _ = tag.save_to_path(&full_path, lofty::config::WriteOptions::default());
                }
            }
        }
    });

    // Fetch the updated track
    let track = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE id = ?")
    )
    .bind(id)
    .fetch_one(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Broadcast track.updated
    let payload_val = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
    let payload_str = payload_val.to_string();
    let event_type = "track.updated";

    if let Ok(er) = sqlx::query("INSERT INTO events (event_type, payload) VALUES (?, ?)")
        .bind(event_type)
        .bind(&payload_str)
        .execute(&user_pool)
        .await
    {
        let event_id = er.last_insert_rowid();
        state.event_bus.broadcast(ServerEvent {
            id: event_id,
            event_type: event_type.to_string(),
            payload: payload_val,
            created_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    Ok(track)
}

pub async fn update_track_metadata(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateMetadataRequest>,
) -> Result<Json<TrackResponse>, (StatusCode, String)> {
    let track = update_track_metadata_inner(&state, &claims, id, payload).await?;
    Ok(Json(track))
}

pub async fn fetch_track_metadata_inner(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    id: i64,
    provider: &str,
) -> Result<TrackResponse, (StatusCode, String)> {
    // 1. Get track path and metadata from DB
    let track_row = sqlx::query("SELECT id, path, title, artist, album, duration FROM tracks WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Track not found".to_string()))?;

    let track_path: String = track_row.get("path");
    let current_title: Option<String> = track_row.get("title");
    let current_artist: Option<String> = track_row.get("artist");
    let current_album: Option<String> = track_row.get("album");
    let duration: Option<i32> = track_row.get("duration");

    let parsed = crate::api::library::parse_track_info(
        &track_path,
        current_title.as_deref(),
        current_artist.as_deref(),
        current_album.as_deref()
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    if provider == "musicbrainz" {
        let mb_match = crate::api::library::fetch_musicbrainz_metadata(
            &client,
            &parsed.title,
            parsed.artist.as_deref(),
            parsed.album.as_deref(),
            duration,
        )
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, "No matching metadata found on MusicBrainz".to_string()))?;

        let album_id = crate::api::library::match_or_create_album(pool, &mb_match.album, &mb_match.artist).await;
        let metadata_json = serde_json::to_string(&mb_match).ok();

        // Update database row
        sqlx::query(
            "UPDATE tracks 
             SET title = ?, artist = ?, album = ?, album_id = ?, track_number = ?, disc_number = ?, genre = ?, external_id = ?, metadata_json = ?,
                 duration = CASE WHEN duration IS NULL OR duration <= 0 THEN ? ELSE duration END
             WHERE id = ?"
        )
        .bind(&mb_match.title)
        .bind(&mb_match.artist)
        .bind(&mb_match.album)
        .bind(album_id)
        .bind(mb_match.track_number)
        .bind(mb_match.disc_number)
        .bind(&mb_match.genre)
        .bind(&mb_match.recording_id)
        .bind(metadata_json)
        .bind(mb_match.duration)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Fetch cover art if available
        if let Some(ref release_id) = mb_match.release_id {
            let cover_url = format!("https://coverartarchive.org/release/{}/front-500", release_id);
            if let Ok(c_resp) = client.get(&cover_url)
                .header("User-Agent", "Audion/0.1.0 ( contact@audion.local )")
                .send()
                .await
            {
                if c_resp.status().is_success() {
                    if let Ok(bytes) = c_resp.bytes().await {
                        let relative_cover = if let Some(parent) = std::path::Path::new(&track_path).parent() {
                            if let Some(grandparent) = parent.parent() {
                                grandparent.join("artwork").join(format!("track_{}.jpg", id)).to_string_lossy().to_string().replace("\\", "/")
                            } else {
                                format!("artwork/track_{}.jpg", id)
                            }
                        } else {
                            format!("artwork/track_{}.jpg", id)
                        };
                        let cover_full = state.config.data_dir.join(&relative_cover);
                        std::fs::create_dir_all(cover_full.parent().unwrap()).ok();
                        let write_success = {
                            let storage = state.storage_backend.read().await;
                            match &*storage {
                                crate::storage::StorageBackend::Local { .. } => {
                                    std::fs::write(&cover_full, &bytes).is_ok()
                                }
                                crate::storage::StorageBackend::S3 { .. }
                                | crate::storage::StorageBackend::Azure { .. } => {
                                    storage.put_object(&relative_cover, bytes.to_vec(), "image/jpeg").await.is_ok()
                                }
                            }
                        };
                        if write_success {
                            sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                                .bind(&relative_cover)
                                .bind(id)
                                .execute(pool)
                                .await
                                .ok();
                        }
                    }
                }
            }
        }

        // Tag writer block (Lofty)
        let file_full_path = state.config.data_dir.join(&track_path);
        let t_title = mb_match.title.clone();
        let t_artist = mb_match.artist.clone();
        let t_album = mb_match.album.clone();
        let t_track_number = mb_match.track_number;
        let t_disc_number = mb_match.disc_number;
        let t_genre = mb_match.genre.clone();
        let t_recording_id = mb_match.recording_id.clone();

        tokio::task::spawn_blocking(move || {
            if file_full_path.exists() {
                use lofty::prelude::*;
                if let Ok(mut tagged_file) = lofty::probe::Probe::open(&file_full_path)
                    .and_then(|p| p.options(lofty::config::ParseOptions::new().parsing_mode(lofty::config::ParsingMode::Relaxed)).read())
                {
                    let tag = if tagged_file.primary_tag().is_some() {
                        tagged_file.primary_tag_mut()
                    } else {
                        tagged_file.first_tag_mut()
                    };
                    if let Some(tag) = tag {
                        tag.insert_text(lofty::tag::ItemKey::TrackTitle, t_title);
                        tag.insert_text(lofty::tag::ItemKey::TrackArtist, t_artist);
                        tag.insert_text(lofty::tag::ItemKey::AlbumTitle, t_album);
                        if let Some(tn) = t_track_number {
                            tag.insert_text(lofty::tag::ItemKey::TrackNumber, tn.to_string());
                        }
                        if let Some(dn) = t_disc_number {
                            tag.insert_text(lofty::tag::ItemKey::DiscNumber, dn.to_string());
                        }
                        if let Some(g) = t_genre {
                            tag.insert_text(lofty::tag::ItemKey::Genre, g);
                        }
                        tag.insert_text(lofty::tag::ItemKey::MusicBrainzTrackId, t_recording_id);
                        let _ = tag.save_to_path(&file_full_path, lofty::config::WriteOptions::default());
                    }
                }
            }
        });
    } else {
        // Deezer fetch
        let url = "https://api.deezer.com/search";
        let query_param = if let Some(ref artist) = parsed.artist {
            format!("{} {}", parsed.title, artist)
        } else {
            parsed.title.clone()
        };

        let resp = client.get(url)
            .query(&[("q", &query_param)])
            .send()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Deezer request failed: {}", e)))?;

        let search_res = resp.json::<crate::api::library::DeezerSearchResponse>().await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to parse Deezer response: {}", e)))?;

        let mut best_match: Option<(&crate::api::library::DeezerTrack, f64)> = None;
        for candidate in search_res.data.iter() {
            let score = crate::api::library::calculate_match_score(
                &parsed.title,
                parsed.artist.as_deref(),
                parsed.album.as_deref(),
                duration,
                &candidate.title,
                &candidate.artist.name,
                &candidate.album.title,
                candidate.duration,
            );
            if score >= 0.5 {
                if best_match.is_none() || score > best_match.as_ref().unwrap().1 {
                    best_match = Some((candidate, score));
                }
            }
        }

        let match_track = best_match
            .map(|(t, _)| t)
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No matching metadata found on Deezer above confidence threshold".to_string()))?;
        let album_id = crate::api::library::match_or_create_album(pool, &match_track.album.title, &match_track.artist.name).await;
        let metadata_json = serde_json::to_value(&match_track).ok().map(|v| v.to_string());
        let ext_id = Some(match_track.id.to_string());

        sqlx::query(
            "UPDATE tracks 
             SET title = ?, artist = ?, album = ?, album_id = ?, external_id = ?, metadata_json = ?,
                 duration = CASE WHEN duration IS NULL OR duration <= 0 THEN ? ELSE duration END
             WHERE id = ?"
        )
        .bind(&match_track.title)
        .bind(&match_track.artist.name)
        .bind(&match_track.album.title)
        .bind(album_id)
        .bind(ext_id)
        .bind(metadata_json)
        .bind(match_track.duration)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Fetch cover art
        if let Some(ref cover_url) = match_track.album.cover_big {
            if let Ok(c_resp) = client.get(cover_url).send().await {
                if c_resp.status().is_success() {
                    if let Ok(bytes) = c_resp.bytes().await {
                        let relative_cover = if let Some(parent) = std::path::Path::new(&track_path).parent() {
                            if let Some(grandparent) = parent.parent() {
                                grandparent.join("artwork").join(format!("track_{}.jpg", id)).to_string_lossy().to_string().replace("\\", "/")
                            } else {
                                format!("artwork/track_{}.jpg", id)
                            }
                        } else {
                            format!("artwork/track_{}.jpg", id)
                        };
                        let cover_full = state.config.data_dir.join(&relative_cover);
                        std::fs::create_dir_all(cover_full.parent().unwrap()).ok();
                        let write_success = {
                            let storage = state.storage_backend.read().await;
                            match &*storage {
                                crate::storage::StorageBackend::Local { .. } => {
                                    std::fs::write(&cover_full, &bytes).is_ok()
                                }
                                crate::storage::StorageBackend::S3 { .. }
                                | crate::storage::StorageBackend::Azure { .. } => {
                                    storage.put_object(&relative_cover, bytes.to_vec(), "image/jpeg").await.is_ok()
                                }
                            }
                        };
                        if write_success {
                            sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                                .bind(&relative_cover)
                                .bind(id)
                                .execute(pool)
                                .await
                                .ok();
                        }
                    }
                }
            }
        }

        // Tag writer block (Lofty)
        let file_full_path = state.config.data_dir.join(&track_path);
        let t_title = match_track.title.clone();
        let t_artist = match_track.artist.name.clone();
        let t_album = match_track.album.title.clone();

        tokio::task::spawn_blocking(move || {
            if file_full_path.exists() {
                use lofty::prelude::*;
                if let Ok(mut tagged_file) = lofty::probe::Probe::open(&file_full_path)
                    .and_then(|p| p.options(lofty::config::ParseOptions::new().parsing_mode(lofty::config::ParsingMode::Relaxed)).read())
                {
                    let tag = if tagged_file.primary_tag().is_some() {
                        tagged_file.primary_tag_mut()
                    } else {
                        tagged_file.first_tag_mut()
                    };
                    if let Some(tag) = tag {
                        tag.insert_text(lofty::tag::ItemKey::TrackTitle, t_title);
                        tag.insert_text(lofty::tag::ItemKey::TrackArtist, t_artist);
                        tag.insert_text(lofty::tag::ItemKey::AlbumTitle, t_album);
                        let _ = tag.save_to_path(&file_full_path, lofty::config::WriteOptions::default());
                    }
                }
            }
        });
    }

    // Get the updated track
    let track = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE id = ?")
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Broadcast track.updated
    let payload_val = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
    let payload_str = payload_val.to_string();
    let event_type = "track.updated";

    if let Ok(er) = sqlx::query("INSERT INTO events (event_type, payload) VALUES (?, ?)")
        .bind(event_type)
        .bind(&payload_str)
        .execute(pool)
        .await
    {
        let event_id = er.last_insert_rowid();
        state.event_bus.broadcast(ServerEvent {
            id: event_id,
            event_type: event_type.to_string(),
            payload: payload_val,
            created_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    Ok(track)
}

pub async fn fetch_track_metadata(
    claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<SingleFetchRequest>,
) -> Result<Json<TrackResponse>, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let track = fetch_track_metadata_inner(&state, &user_pool, id, &payload.provider).await?;
    Ok(Json(track))
}

async fn broadcast_bulk_event(state: &AppState, pool: &sqlx::SqlitePool, event_type: &str, payload: serde_json::Value) {
    let payload_str = payload.to_string();
    if let Ok(er) = sqlx::query("INSERT INTO events (event_type, payload) VALUES (?, ?)")
        .bind(event_type)
        .bind(&payload_str)
        .execute(pool)
        .await
    {
        let event_id = er.last_insert_rowid();
        state.event_bus.broadcast(ServerEvent {
            id: event_id,
            event_type: event_type.to_string(),
            payload,
            created_at: chrono::Utc::now().to_rfc3339(),
        });
    }
}

pub async fn bulk_fetch_metadata(
    claims: Claims,
    State(state): State<AppState>,
    Json(payload): Json<BulkFetchRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let total = payload.track_ids.len();
    for (idx, id) in payload.track_ids.iter().copied().enumerate() {
        if let Err(e) = fetch_track_metadata_inner(&state, &user_pool, id, &payload.provider).await {
            error!("Failed to fetch metadata for track {}: {:?}", id, e);
        }
        
        let progress_payload = serde_json::json!({
            "action": "fetch",
            "current": idx + 1,
            "total": total,
            "track_id": id,
        });
        broadcast_bulk_event(&state, &user_pool, "bulk.progress", progress_payload).await;

        // Yield execution to avoid rate limits
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let completed_payload = serde_json::json!({
        "action": "fetch",
        "total": total,
    });
    broadcast_bulk_event(&state, &user_pool, "bulk.completed", completed_payload).await;

    Ok(StatusCode::OK)
}

pub async fn bulk_delete_tracks(
    claims: Claims,
    State(state): State<AppState>,
    Json(payload): Json<BulkDeleteRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let total = payload.track_ids.len();
    for (idx, id) in payload.track_ids.iter().copied().enumerate() {
        if let Err(e) = delete_track_inner(&state, &user_pool, id).await {
            error!("Failed to delete track {}: {:?}", id, e);
        }
        
        let progress_payload = serde_json::json!({
            "action": "delete",
            "current": idx + 1,
            "total": total,
            "track_id": id,
        });
        broadcast_bulk_event(&state, &user_pool, "bulk.progress", progress_payload).await;
    }

    let completed_payload = serde_json::json!({
        "action": "delete",
        "total": total,
    });
    broadcast_bulk_event(&state, &user_pool, "bulk.completed", completed_payload).await;

    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bulk_payload_deserialization() {
        let json_data = r#"{"track_ids": [1, 2, 3], "provider": "musicbrainz"}"#;
        let req: BulkFetchRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(req.track_ids, vec![1, 2, 3]);
        assert_eq!(req.provider, "musicbrainz");

        let json_del = r#"{"track_ids": [10, 20]}"#;
        let req_del: BulkDeleteRequest = serde_json::from_str(json_del).unwrap();
        assert_eq!(req_del.track_ids, vec![10, 20]);
    }
}

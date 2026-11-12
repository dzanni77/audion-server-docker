use super::*;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DeezerArtist {
    pub name: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DeezerAlbum {
    pub title: String,
    #[serde(rename = "cover_big")]
    pub cover_big: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DeezerTrack {
    pub id: i64,
    pub title: String,
    pub artist: DeezerArtist,
    pub album: DeezerAlbum,
    pub duration: Option<i32>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DeezerSearchResponse {
    pub data: Vec<DeezerTrack>,
}

// MusicBrainz structs
#[derive(Deserialize, Debug, Clone)]
pub struct MbArtist {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbArtistCredit {
    pub artist: MbArtist,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbTrack {
    pub number: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbMedia {
    #[serde(rename = "track-count")]
    pub _track_count: Option<i32>,
    pub tracks: Option<Vec<MbTrack>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbRelease {
    pub id: String,
    pub title: String,
    pub date: Option<String>,
    pub media: Option<Vec<MbMedia>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbTag {
    pub name: String,
    pub count: i32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbRecording {
    pub id: String,
    pub title: String,
    #[serde(rename = "artist-credit")]
    pub artist_credit: Option<Vec<MbArtistCredit>>,
    pub releases: Option<Vec<MbRelease>>,
    pub tags: Option<Vec<MbTag>>,
    pub length: Option<i32>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MbSearchResponse {
    pub recordings: Vec<MbRecording>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MusicBrainzMatch {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub genre: Option<String>,
    pub release_id: Option<String>,
    pub recording_id: String,
    pub duration: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ParsedTrackQuery {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

pub async fn get_fetch_status(
    claims: Claims,
    State(state): State<AppState>,
) -> Json<FetchStatusResponse> {
    let status_lock = state.get_user_fetcher_status(&claims.sub).await;
    let status = status_lock.lock().unwrap();
    Json(FetchStatusResponse {
        is_running: status.is_running,
        tracks_processed: status.tracks_processed,
        total_tracks: status.total_tracks,
        current_track: status.current_track.clone(),
        logs: status.logs.clone(),
    })
}

pub async fn fetch_musicbrainz_metadata(
    client: &reqwest::Client,
    target_title: &str,
    target_artist: Option<&str>,
    target_album: Option<&str>,
    target_duration: Option<i32>,
) -> Option<MusicBrainzMatch> {
    let url = "https://musicbrainz.org/ws/2/recording";
    
    // 1. Build a specific targeted query
    let mut query_parts = Vec::new();
    query_parts.push(format!("recording:\"{}\"", target_title.replace(":", " ").replace("-", " ").replace("\"", " ")));
    if let Some(artist) = target_artist {
        query_parts.push(format!("artist:\"{}\"", artist.replace(":", " ").replace("-", " ").replace("\"", " ")));
    }
    if let Some(album) = target_album {
        query_parts.push(format!("release:\"{}\"", album.replace(":", " ").replace("-", " ").replace("\"", " ")));
    }
    let query_param = query_parts.join(" AND ");

    let mut response = client.get(url)
        .header("User-Agent", "Audion/0.1.0 ( contact@audion.local )")
        .query(&[("query", query_param.as_str()), ("fmt", "json"), ("limit", "10")])
        .send()
        .await
        .ok()?;

    let mut search_res = if response.status().is_success() {
        response.json::<MbSearchResponse>().await.ok()
    } else {
        None
    };

    // 2. Fallback: if we got no results, try a broader query (just title and artist)
    if search_res.is_none() || search_res.as_ref().unwrap().recordings.is_empty() {
        if target_artist.is_some() {
            let broad_query = format!("{} {}", target_title, target_artist.unwrap());
            let clean_broad = broad_query.replace(":", " ").replace("-", " ").replace("\"", " ");
            response = client.get(url)
                .header("User-Agent", "Audion/0.1.0 ( contact@audion.local )")
                .query(&[("query", clean_broad.as_str()), ("fmt", "json"), ("limit", "10")])
                .send()
                .await
                .ok()?;

            if response.status().is_success() {
                search_res = response.json::<MbSearchResponse>().await.ok();
            }
        }
    }

    let search_res = search_res?;
    if search_res.recordings.is_empty() {
        return None;
    }

    // 3. Iterate candidates, score them, and pick the best candidate above confidence threshold
    let mut best_match: Option<(MusicBrainzMatch, f64)> = None;

    for recording in search_res.recordings.iter() {
        let title = recording.title.clone();
        
        let artist = recording.artist_credit.as_ref()
            .and_then(|ac| ac.first())
            .map(|c| c.artist.name.clone())
            .unwrap_or_else(|| "Unknown Artist".to_string());
            
        let release = recording.releases.as_ref().and_then(|r| r.first());
        let album = release.map(|r| r.title.clone()).unwrap_or_else(|| "Unknown Album".to_string());
        let release_id = release.map(|r| r.id.clone());
        
        let mut track_number = None;
        let mut disc_number = None;
        
        if let Some(r) = release {
            if let Some(ref media) = r.media {
                for (m_idx, m) in media.iter().enumerate() {
                    if let Some(ref tracks) = m.tracks {
                        for t in tracks {
                            if let Some(ref num) = t.number {
                                if let Ok(n) = num.parse::<i32>() {
                                    track_number = Some(n);
                                    disc_number = Some((m_idx + 1) as i32);
                                    break;
                                }
                            }
                        }
                    }
                    if track_number.is_some() {
                        break;
                    }
                }
            }
        }
        
        let genre = recording.tags.as_ref()
            .and_then(|tags| {
                tags.iter()
                    .max_by_key(|t| t.count)
                    .map(|t| t.name.clone())
            });

        let duration = recording.length.map(|l| (l / 1000) as i32);

        let candidate_match = MusicBrainzMatch {
            title: title.clone(),
            artist: artist.clone(),
            album: album.clone(),
            track_number,
            disc_number,
            genre,
            release_id,
            recording_id: recording.id.clone(),
            duration,
        };

        // Score this candidate
        let score = calculate_match_score(
            target_title,
            target_artist,
            target_album,
            target_duration,
            &title,
            &artist,
            &album,
            duration,
        );

        if score >= 0.5 { // Match confidence threshold
            if best_match.is_none() || score > best_match.as_ref().unwrap().1 {
                best_match = Some((candidate_match, score));
            }
        }
    }

    best_match.map(|(m, _)| m)
}

pub async fn start_metadata_fetcher(
    claims: Claims,
    State(state): State<AppState>,
    payload: Option<Json<FetcherRequest>>,
) -> Result<StatusCode, (StatusCode, String)> {
    claims.require_non_stream_only().map_err(|(s, m)| (s, m.to_string()))?;
    let user_pool = state.get_user_pool(&claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let fetcher_status_lock = state.get_user_fetcher_status(&claims.sub).await;
    let mut status = fetcher_status_lock.lock().unwrap();
    if status.is_running {
        return Err((StatusCode::CONFLICT, "Metadata fetcher already running".to_string()));
    }

    let provider = payload
        .and_then(|Json(p)| p.provider)
        .unwrap_or_else(|| "deezer".to_string());

    status.is_running = true;
    status.tracks_processed = 0;
    status.total_tracks = 0;
    status.current_track = None;
    status.logs = vec![format!("Worker started (Provider: {}). Analyzing database library...", provider)];

    let pool = user_pool;
    let config = state.config.clone();
    let event_bus = state.event_bus.clone();
    let fetcher_status = fetcher_status_lock.clone();
    let user_id = claims.sub.clone();

    let provider_clone = provider.clone();
    tokio::spawn(async move {
        // Query tracks missing metadata (unknown titles or missing artists/albums)
        let tracks_res = sqlx::query(
            "SELECT id, path, title, artist, album, duration FROM tracks 
             WHERE artist IS NULL OR album IS NULL OR title IS NULL OR title = '' OR title LIKE 'music/%'"
        )
        .fetch_all(&pool)
        .await;

        let tracks = match tracks_res {
            Ok(t) => t,
            Err(e) => {
                let err_msg = format!("Database error while querying tracks: {}", e);
                log_fetcher_message(&fetcher_status, &event_bus, &err_msg);
                let mut status = fetcher_status.lock().unwrap();
                status.is_running = false;
                return;
            }
        };

        let total = tracks.len();
        {
            let mut status = fetcher_status.lock().unwrap();
            status.total_tracks = total;
        }

        let start_msg = format!("Found {} tracks needing metadata completion.", total);
        log_fetcher_message(&fetcher_status, &event_bus, &start_msg);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        for (idx, row) in tracks.iter().enumerate() {
            let track_id: i64 = row.get("id");
            let track_path: String = row.get("path");
            let current_title: Option<String> = row.get("title");
            let current_artist: Option<String> = row.get("artist");
            let current_album: Option<String> = row.get("album");
            let duration: Option<i32> = row.get("duration");

            let parsed = parse_track_info(&track_path, current_title.as_deref(), current_artist.as_deref(), current_album.as_deref());

            {
                let mut status = fetcher_status.lock().unwrap();
                status.tracks_processed = idx + 1;
                status.current_track = Some(parsed.title.clone());
            }

            let search_msg = format!("[{}/{}] Searching for: {}", idx + 1, total, parsed.title);
            log_fetcher_message(&fetcher_status, &event_bus, &search_msg);

            let provider_run = provider_clone.clone();

            if provider_run == "musicbrainz" {
                let mb_match_opt = fetch_musicbrainz_metadata(
                    &client,
                    &parsed.title,
                    parsed.artist.as_deref(),
                    parsed.album.as_deref(),
                    duration,
                ).await;

                match mb_match_opt {
                    Some(mb_match) => {
                        let found_msg = format!(
                            "  -> Match found (MusicBrainz): \"{}\" by \"{}\" (Album: \"{}\")",
                            mb_match.title, mb_match.artist, mb_match.album
                        );
                        log_fetcher_message(&fetcher_status, &event_bus, &found_msg);

                        // Match album
                        let album_id = match_or_create_album(&pool, &mb_match.album, &mb_match.artist).await;

                        // Serialize full raw response as metadata_json
                        let metadata_json = serde_json::to_string(&mb_match).ok();

                        // Update track metadata in DB (including new fields: track_number, disc_number, genre, external_id, metadata_json, and duration if missing)
                        let update_res = sqlx::query(
                            "UPDATE tracks SET title = ?, artist = ?, album = ?, album_id = ?, track_number = ?, disc_number = ?, genre = ?, external_id = ?, metadata_json = ?, duration = CASE WHEN duration IS NULL OR duration <= 0 THEN ? ELSE duration END WHERE id = ?"
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
                        .bind(track_id)
                        .execute(&pool)
                        .await;

                        if update_res.is_ok() {
                            // Try downloading album artwork from Cover Art Archive
                            if let Some(ref release_id) = mb_match.release_id {
                                let cover_url = format!("https://coverartarchive.org/release/{}/front-500", release_id);
                                let c_resp_res = client.get(&cover_url)
                                    .header("User-Agent", "Audion/0.1.0 ( contact@audion.local )")
                                    .send()
                                    .await;

                                if let Ok(c_resp) = c_resp_res {
                                    if c_resp.status().is_success() {
                                        if let Ok(bytes) = c_resp.bytes().await {
                                            let relative_cover = format!("users/{}/artwork/track_{}.jpg", user_id, track_id);
                                            let cover_full = config.data_dir.join(&relative_cover);
                                            std::fs::create_dir_all(cover_full.parent().unwrap()).ok();
                                            if std::fs::write(&cover_full, &bytes).is_ok() {
                                                sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                                                    .bind(&relative_cover)
                                                    .bind(track_id)
                                                    .execute(&pool)
                                                    .await
                                                    .ok();
                                            }

                                            if let Some(alb_id) = album_id {
                                                let has_art = sqlx::query("SELECT art_path FROM albums WHERE id = ? AND art_path IS NOT NULL")
                                                    .bind(alb_id)
                                                    .fetch_optional(&pool)
                                                    .await
                                                    .map(|o| o.is_some())
                                                    .unwrap_or(false);

                                                if !has_art {
                                                    let relative_art = format!("users/{}/artwork/album_{}.jpg", user_id, alb_id);
                                                    let art_full = config.data_dir.join(&relative_art);
                                                    if std::fs::write(&art_full, &bytes).is_ok() {
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
                                    }
                                }
                            }

                            // Try writing tags back to file asynchronously
                            let file_full_path = config.data_dir.join(&track_path);
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

                            // Broadcast track.updated
                            if let Ok(track) = sqlx::query_as::<_, TrackResponse>(
                                &track_select_sql("WHERE id = ?")
                            )
                            .bind(track_id)
                            .fetch_one(&pool)
                            .await {
                                let payload = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
                                broadcast_library_event(&event_bus, "track.updated", payload);
                            }
                        }
                    }
                    None => {
                        log_fetcher_message(&fetcher_status, &event_bus, "  -> No match found in MusicBrainz.");
                    }
                }
            } else {
                // Fetch from Deezer API
                let url = "https://api.deezer.com/search";
                let query_param = if let Some(ref artist) = parsed.artist {
                    format!("{} {}", parsed.title, artist)
                } else {
                    parsed.title.clone()
                };

                let response = client.get(url)
                    .query(&[("q", &query_param)])
                    .send()
                    .await;

                match response {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            if let Ok(search_res) = resp.json::<DeezerSearchResponse>().await {
                                let mut best_match: Option<(&DeezerTrack, f64)> = None;
                                for candidate in search_res.data.iter() {
                                    let score = calculate_match_score(
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

                                if let Some((match_track, _)) = best_match {
                                    let found_msg = format!(
                                        "  -> Match found (Deezer): \"{}\" by \"{}\" (Album: \"{}\")",
                                        match_track.title, match_track.artist.name, match_track.album.title
                                    );
                                    log_fetcher_message(&fetcher_status, &event_bus, &found_msg);

                                    // Match album
                                    let album_id = match_or_create_album(&pool, &match_track.album.title, &match_track.artist.name).await;

                                    // Serialize response
                                    let metadata_json = serde_json::to_value(&match_track).ok().map(|v| v.to_string());
                                    let ext_id = Some(match_track.id.to_string());

                                    // Update track in DB
                                    let update_res = sqlx::query(
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
                                    .bind(track_id)
                                    .execute(&pool)
                                    .await;

                                    if update_res.is_ok() {
                                        // Try downloading album artwork
                                        if let Some(ref cover_url) = match_track.album.cover_big {
                                            if let Ok(c_resp) = client.get(cover_url).send().await {
                                                if c_resp.status().is_success() {
                                                    if let Ok(bytes) = c_resp.bytes().await {
                                                        let relative_cover = format!("users/{}/artwork/track_{}.jpg", user_id, track_id);
                                                        let cover_full = config.data_dir.join(&relative_cover);
                                                        std::fs::create_dir_all(cover_full.parent().unwrap()).ok();
                                                        if std::fs::write(&cover_full, &bytes).is_ok() {
                                                            sqlx::query("UPDATE tracks SET track_cover_path = ? WHERE id = ?")
                                                                .bind(&relative_cover)
                                                                .bind(track_id)
                                                                .execute(&pool)
                                                                .await
                                                                .ok();
                                                        }

                                                        if let Some(alb_id) = album_id {
                                                            let has_art = sqlx::query("SELECT art_path FROM albums WHERE id = ? AND art_path IS NOT NULL")
                                                                .bind(alb_id)
                                                                .fetch_optional(&pool)
                                                                .await
                                                                .map(|o| o.is_some())
                                                                .unwrap_or(false);

                                                            if !has_art {
                                                                let relative_art = format!("users/{}/artwork/album_{}.jpg", user_id, alb_id);
                                                                let art_full = config.data_dir.join(&relative_art);
                                                                if std::fs::write(&art_full, &bytes).is_ok() {
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
                                                }
                                            }
                                        }

                                        // Try writing tags back to file asynchronously
                                        let file_full_path = config.data_dir.join(&track_path);
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

                                        // Broadcast track.updated
                                        if let Ok(track) = sqlx::query_as::<_, TrackResponse>(
                                            &track_select_sql("WHERE id = ?")
                                        )
                                        .bind(track_id)
                                        .fetch_one(&pool)
                                        .await {
                                            let payload = serde_json::to_value(&track).unwrap_or(serde_json::Value::Null);
                                            broadcast_library_event(&event_bus, "track.updated", payload);
                                        }
                                    }
                                } else {
                                    log_fetcher_message(&fetcher_status, &event_bus, "  -> No matching track found above confidence threshold on Deezer.");
                                }
                            }
                        } else {
                            let err_log = format!("  -> API returned status error: {}", resp.status());
                            log_fetcher_message(&fetcher_status, &event_bus, &err_log);
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Deezer API request error: {}", e);
                        log_fetcher_message(&fetcher_status, &event_bus, &err_msg);
                    }
                }
            }

            // Throttle requests slightly based on provider
            let sleep_ms = if provider_run == "musicbrainz" { 1000 } else { 500 };
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        }

        {
            let mut status = fetcher_status.lock().unwrap();
            status.is_running = false;
        }
        log_fetcher_message(&fetcher_status, &event_bus, "Worker complete. All tracks processed.");
        
        let final_status = {
            let status = fetcher_status.lock().unwrap();
            status.clone()
        };
        broadcast_library_event(&event_bus, "fetch.completed", serde_json::to_value(&final_status).unwrap_or(serde_json::Value::Null));
    });

    Ok(StatusCode::ACCEPTED)
}

pub fn log_fetcher_message(fetcher_status: &Arc<std::sync::Mutex<FetcherStatus>>, event_bus: &crate::events::EventBus, message: &str) {
    let mut status = fetcher_status.lock().unwrap();
    status.logs.push(message.to_string());
    if status.logs.len() > 30 {
        status.logs.remove(0);
    }

    let payload = serde_json::json!({
        "is_running": status.is_running,
        "tracks_processed": status.tracks_processed,
        "total_tracks": status.total_tracks,
        "current_track": status.current_track.clone(),
        "logs": status.logs.clone(),
    });
    
    broadcast_library_event(event_bus, "fetch.progress", payload);
}

pub fn clean_search_term(input: &str) -> String {
    let mut clean = input.replace(".mp3", "")
        .replace(".flac", "")
        .replace(".m4a", "")
        .replace(".ogg", "")
        .replace(".wav", "")
        .replace(".alac", "")
        .replace(".aac", "")
        .replace('_', " ")
        .replace('-', " ");
    
    let parts: Vec<&str> = clean.split_whitespace().collect();
    if !parts.is_empty() {
        let first = parts[0];
        let is_num = first.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
        if is_num && parts.len() > 1 {
            clean = parts[1..].join(" ");
        }
    }
    
    clean.trim().to_string()
}

pub async fn match_or_create_album(pool: &sqlx::SqlitePool, name: &str, artist: &str) -> Option<i64> {
    if name.trim().is_empty() {
        return None;
    }

    let existing = sqlx::query("SELECT id FROM albums WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);

    if let Some(alb) = existing {
        Some(alb.get::<i64, _>("id"))
    } else {
        let res = sqlx::query("INSERT INTO albums (name, artist) VALUES (?, ?)")
            .bind(name)
            .bind(artist)
            .execute(pool)
            .await;
        
        match res {
            Ok(r) => Some(r.last_insert_rowid()),
            Err(_) => None,
        }
    }
}

pub fn parse_track_info(
    path: &str,
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
) -> ParsedTrackQuery {
    let db_artist = artist.filter(|s| !s.trim().is_empty());
    let db_title = title.filter(|s| !s.trim().is_empty() && !s.starts_with("music/"));
    let db_album = album.filter(|s| !s.trim().is_empty());

    if let (Some(t), Some(a)) = (db_title, db_artist) {
        return ParsedTrackQuery {
            title: t.to_string(),
            artist: Some(a.to_string()),
            album: db_album.map(|s| s.to_string()),
        };
    }

    // Fallback: parse filename
    let filename = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let cleaned_filename = filename.replace('_', " ");

    let mut parts: Vec<&str> = cleaned_filename.split(" - ").collect();
    if !parts.is_empty() {
        let first = parts[0].trim();
        let is_num = first.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
        if is_num && parts.len() > 1 {
            parts.remove(0);
        }
    }

    if parts.len() >= 2 {
        let clean_parts: Vec<String> = parts.iter().map(|p| {
            let mut s = p.trim().to_string();
            let words: Vec<&str> = s.split_whitespace().collect();
            if !words.is_empty() {
                let first = words[0];
                let is_num = first.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
                if is_num && words.len() > 1 {
                    s = words[1..].join(" ");
                }
            }
            s
        }).collect();

        if clean_parts.len() == 2 {
            return ParsedTrackQuery {
                title: clean_parts[1].clone(),
                artist: Some(clean_parts[0].clone()),
                album: db_album.map(|s| s.to_string()),
            };
        } else if clean_parts.len() >= 3 {
            let artist_val = clean_parts[0].clone();
            let title_val = clean_parts[clean_parts.len() - 1].clone();
            let album_val = Some(clean_parts[1].clone());
            return ParsedTrackQuery {
                title: title_val,
                artist: Some(artist_val),
                album: album_val,
            };
        }
    }

    let fallback_title = db_title.map(|s| s.to_string()).unwrap_or_else(|| {
        let mut s = cleaned_filename.trim().to_string();
        let words: Vec<&str> = s.split_whitespace().collect();
        if !words.is_empty() {
            let first = words[0];
            let is_num = first.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
            if is_num && words.len() > 1 {
                s = words[1..].join(" ");
            }
        }
        s
    });

    ParsedTrackQuery {
        title: fallback_title,
        artist: db_artist.map(|s| s.to_string()),
        album: db_album.map(|s| s.to_string()),
    }
}

pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 { return b_len; }
    if b_len == 0 { return a_len; }

    let mut dp = vec![0; b_len + 1];
    for j in 0..=b_len {
        dp[j] = j;
    }

    for i in 1..=a_len {
        let mut prev = dp[0];
        dp[0] = i;
        for j in 1..=b_len {
            let temp = dp[j];
            if a_chars[i - 1] == b_chars[j - 1] {
                dp[j] = prev;
            } else {
                dp[j] = std::cmp::min(
                    prev + 1,
                    std::cmp::min(
                        dp[j] + 1,
                        dp[j - 1] + 1
                    )
                );
            }
            prev = temp;
        }
    }
    dp[b_len]
}

pub fn clean_string_for_compare(s: &str) -> String {
    let mut cleaned = s.to_lowercase();
    cleaned = cleaned.replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), " ");
    
    let stop_words = vec![
        "feat", "featuring", "remastered", "remaster", "live", "acoustic", "version", "hq", "official"
    ];
    for word in stop_words {
        cleaned = cleaned.replace(word, " ");
    }

    // Remove 4-digit years starting with 19 or 20 (e.g., 1990, 2009)
    cleaned = cleaned.split_whitespace()
        .filter(|w| !(w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && (w.starts_with("19") || w.starts_with("20"))))
        .collect::<Vec<&str>>()
        .join(" ");

    cleaned
}

pub fn string_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() { return 1.0; }
    if a.is_empty() || b.is_empty() { return 0.0; }
    
    let clean_a = clean_string_for_compare(a);
    let clean_b = clean_string_for_compare(b);
    
    if clean_a == clean_b { return 1.0; }

    let a_tokens: std::collections::HashSet<&str> = clean_a.split_whitespace().collect();
    let b_tokens: std::collections::HashSet<&str> = clean_b.split_whitespace().collect();

    let intersection = a_tokens.intersection(&b_tokens).count();
    let union = a_tokens.union(&b_tokens).count();

    if union == 0 {
        return 0.0;
    }

    let token_sim = intersection as f64 / union as f64;

    let lev = levenshtein_distance(&clean_a, &clean_b);
    let max_len = std::cmp::max(clean_a.chars().count(), clean_b.chars().count());
    let lev_sim = if max_len == 0 { 0.0 } else { 1.0 - (lev as f64 / max_len as f64) };

    (token_sim * 0.6) + (lev_sim * 0.4)
}

pub fn calculate_match_score(
    target_title: &str,
    target_artist: Option<&str>,
    target_album: Option<&str>,
    target_duration: Option<i32>,
    candidate_title: &str,
    candidate_artist: &str,
    candidate_album: &str,
    candidate_duration: Option<i32>,
) -> f64 {
    let title_score = string_similarity(target_title, candidate_title);

    let artist_score = if let Some(t_artist) = target_artist {
        string_similarity(t_artist, candidate_artist)
    } else {
        1.0
    };

    let album_score = if let Some(t_album) = target_album {
        string_similarity(t_album, candidate_album)
    } else {
        1.0
    };

    let duration_score = if let (Some(t_dur), Some(c_dur)) = (target_duration, candidate_duration) {
        let diff = (t_dur - c_dur).abs();
        if diff <= 2 {
            1.0
        } else if diff <= 5 {
            0.8
        } else if diff <= 10 {
            0.5
        } else if diff <= 20 {
            0.2
        } else {
            0.0
        }
    } else {
        1.0
    };

    let mut score = 0.0;
    if target_artist.is_some() {
        if target_album.is_some() {
            score += title_score * 0.40;
            score += artist_score * 0.40;
            score += album_score * 0.10;
            score += duration_score * 0.10;
        } else {
            score += title_score * 0.45;
            score += artist_score * 0.45;
            score += duration_score * 0.10;
        }
    } else {
        if target_album.is_some() {
            score += title_score * 0.70;
            score += album_score * 0.10;
            score += duration_score * 0.20;
        } else {
            score += title_score * 0.80;
            score += duration_score * 0.20;
        }
    }

    score
}

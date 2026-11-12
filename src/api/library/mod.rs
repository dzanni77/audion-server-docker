use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, error, warn};
use sqlx::Row;
use lofty::prelude::*;
use lofty::file::TaggedFileExt;

use crate::state::{AppState, ScanStatus, FetcherStatus};
use crate::auth::Claims;
use crate::scanner::extract_metadata;
use crate::events::ServerEvent;
use crate::api::tracks::{track_select_sql, TrackResponse};

pub mod scan;
pub mod fetch;
pub mod management;

pub use scan::{get_scan_status, start_scan, trigger_auto_scan};
pub use fetch::{
    get_fetch_status, start_metadata_fetcher,
    DeezerArtist, DeezerAlbum, DeezerTrack, DeezerSearchResponse,
    MbArtist, MbArtistCredit, MbTrack, MbMedia, MbRelease, MbTag, MbRecording, MbSearchResponse,
    MusicBrainzMatch, ParsedTrackQuery,
    fetch_musicbrainz_metadata, parse_track_info, levenshtein_distance, string_similarity, calculate_match_score,
    clean_string_for_compare, clean_search_term, match_or_create_album,
};
pub use management::{clean_library, reset_library};

#[derive(Serialize)]
pub struct LibraryStatusResponse {
    pub is_scanning: bool,
    pub files_scanned: usize,
    pub total_files: usize,
    pub current_file: Option<String>,
}

#[derive(Serialize)]
pub struct FetchStatusResponse {
    pub is_running: bool,
    pub tracks_processed: usize,
    pub total_tracks: usize,
    pub current_track: Option<String>,
    pub logs: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub struct FetcherRequest {
    pub provider: Option<String>,
}

pub fn broadcast_library_event(event_bus: &crate::events::EventBus, event_type: &str, payload: serde_json::Value) {
    let time_now = chrono::Utc::now().to_rfc3339();
    event_bus.broadcast(ServerEvent {
        id: 0,
        event_type: event_type.to_string(),
        payload,
        created_at: time_now,
    });
}

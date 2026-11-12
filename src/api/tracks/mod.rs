use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::path::PathBuf;
use tracing::{info, error};
use sqlx::Row;

use crate::state::AppState;
use crate::auth::Claims;
use crate::scanner::extract_metadata;
use crate::events::ServerEvent;

pub mod upload;
pub mod metadata;
pub mod query;
pub mod lyrics;

pub use upload::upload_track;
pub use metadata::{update_track_metadata, fetch_track_metadata, bulk_fetch_metadata, bulk_update_metadata, UpdateMetadataRequest, BulkMetadataUpdateRequest, bulk_delete_tracks};
pub use query::{get_tracks, get_track_by_id, delete_track, delete_track_inner};
pub use lyrics::get_track_lyrics;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct TrackResponse {
    pub id: i64,
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub year: Option<String>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub duration: Option<i32>,
    pub album_id: Option<i64>,
    pub format: Option<String>,
    pub bitrate: Option<i32>,
    pub source_type: Option<String>,
    pub cover_url: Option<String>,
    pub external_id: Option<String>,
    pub local_src: Option<String>,
    pub track_cover_path: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i32>,
    pub isrc: Option<String>,
    pub lyrics: Option<String>,
    pub metadata_json: Option<String>,
    pub date_added: Option<String>,
}

/// Explicit SQL column list matching [`TrackResponse`] field-for-field.
///
/// Keeping this projection centralized next to [`TrackResponse`] prevents
/// `sqlx::Error::ColumnNotFound` drift when fields are added to `tracks`.
pub const TRACK_SELECT_COLUMNS: &str = "\
    id, path, title, artist, album, album_artist, composer, year, \
    track_number, disc_number, duration, album_id, format, bitrate, \
    source_type, cover_url, external_id, local_src, track_cover_path, \
    genre, comment, bpm, isrc, lyrics, metadata_json, date_added";

/// Table-aliased (`t.`) SQL column list matching [`TrackResponse`] for joined queries.
pub const TRACK_SELECT_COLUMNS_ALIASED: &str = "\
    t.id, t.path, t.title, t.artist, t.album, t.album_artist, t.composer, t.year, \
    t.track_number, t.disc_number, t.duration, t.album_id, t.format, t.bitrate, \
    t.source_type, t.cover_url, t.external_id, t.local_src, t.track_cover_path, \
    t.genre, t.comment, t.bpm, t.isrc, t.lyrics, t.metadata_json, t.date_added";

/// Builds a `SELECT ... FROM tracks {clause}` query covering all [`TrackResponse`] columns.
pub fn track_select_sql(clause: &str) -> String {
    format!("SELECT {TRACK_SELECT_COLUMNS} FROM tracks {clause}")
}

/// Builds a `SELECT ... FROM tracks t {clause}` query for joined lookups on `tracks t`.
pub fn track_select_aliased_sql(clause: &str) -> String {
    format!("SELECT {TRACK_SELECT_COLUMNS_ALIASED} FROM tracks t {clause}")
}

#[derive(Deserialize)]
pub struct PaginatedQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use sqlx::sqlite::SqlitePool;

    #[test]
    fn track_select_projections_stay_in_sync() {
        let plain: Vec<&str> = TRACK_SELECT_COLUMNS.split(',').map(str::trim).collect();
        let aliased: Vec<String> = TRACK_SELECT_COLUMNS_ALIASED
            .split(',')
            .map(|c| c.trim().trim_start_matches("t.").to_string())
            .collect();
        assert_eq!(plain.len(), aliased.len());
        for (p, a) in plain.iter().zip(aliased.iter()) {
            assert_eq!(p, a);
        }
    }

    #[tokio::test]
    async fn track_select_queries_deserialize_against_schema() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_db(&pool).await.unwrap();

        sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ('u1', 'tester', 'hash')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO albums (id, name, artist) VALUES (1, 'Language', 'The Contortionist')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, path, title, artist, album, album_artist, album_id)
             VALUES (1, 'users/u1/music/01.flac', 'The Source', 'The Contortionist', 'Language', 'The Contortionist', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO liked_tracks (user_id, track_id) VALUES ('u1', 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES (1, 'u1', 'Prog')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (1, 1, 0)")
            .execute(&pool)
            .await
            .unwrap();

        let single = sqlx::query_as::<_, TrackResponse>(&track_select_sql("WHERE id = ?"))
            .bind(1)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(single.title.as_deref(), Some("The Source"));

        let liked = sqlx::query_as::<_, TrackResponse>(&track_select_aliased_sql(
            "JOIN liked_tracks lt ON lt.track_id = t.id WHERE lt.user_id = ?",
        ))
        .bind("u1")
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(liked.len(), 1);

        let playlist = sqlx::query_as::<_, TrackResponse>(&track_select_aliased_sql(
            "JOIN playlist_tracks pt ON pt.track_id = t.id WHERE pt.playlist_id = ?",
        ))
        .bind(1)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(playlist.len(), 1);
    }
}

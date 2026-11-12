use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_sql, TrackResponse};
use crate::api::albums::AlbumResponse;
use crate::api::artists::ArtistResponse;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

#[derive(Serialize)]
pub struct SearchResults {
    pub tracks: Vec<TrackResponse>,
    pub albums: Vec<AlbumResponse>,
    pub artists: Vec<ArtistResponse>,
}

pub async fn search(
    _claims: Claims,
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResults>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let search_pattern = format!("%{}%", query.q);

    // Search tracks
    let tracks = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE title LIKE ? OR artist LIKE ? OR album LIKE ? OR genre LIKE ? ORDER BY artist, album, disc_number, track_number, title LIMIT 50")
    )
    .bind(&search_pattern)
    .bind(&search_pattern)
    .bind(&search_pattern)
    .bind(&search_pattern)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Search albums
    let albums = sqlx::query_as::<_, AlbumResponse>(
        "SELECT id, name, artist, art_path 
         FROM albums 
         WHERE name LIKE ? OR artist LIKE ?
         ORDER BY artist, name
         LIMIT 20"
    )
    .bind(&search_pattern)
    .bind(&search_pattern)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Search artists
    let artists = sqlx::query_as::<_, ArtistResponse>(
        "SELECT artist as name, COUNT(*) as track_count, COUNT(DISTINCT album) as album_count 
         FROM tracks 
         WHERE artist LIKE ?
         GROUP BY artist 
         ORDER BY artist
         LIMIT 20"
    )
    .bind(&search_pattern)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SearchResults {
        tracks,
        albums,
        artists,
    }))
}

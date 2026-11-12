use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_sql, TrackResponse};
use crate::api::albums::AlbumResponse;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ArtistResponse {
    pub name: String,
    pub track_count: i32,
    pub album_count: i32,
}

pub async fn get_artists(
    _claims: Claims,
    State(state): State<AppState>,
) -> Result<Json<Vec<ArtistResponse>>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let artists = sqlx::query_as::<_, ArtistResponse>(
        "SELECT artist as name, COUNT(*) as track_count, COUNT(DISTINCT album) as album_count 
         FROM tracks 
         WHERE artist IS NOT NULL 
         GROUP BY artist 
         ORDER BY artist"
    )
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(artists))
}

/// Returns the raw path parameter alongside a `+`-to-space normalized variant.
///
/// Axum's `Path` extractor decodes percent-encoded spaces (`%20`) but preserves literal `+`
/// characters sent by some form-encoding clients. Matching both variants keeps `+`-encoded
/// requests working without breaking artists whose literal name contains `+`.
fn artist_name_variants(artist_name: &str) -> (&str, String) {
    (artist_name, artist_name.replace('+', " "))
}

pub async fn get_artist_albums(
    _claims: Claims,
    State(state): State<AppState>,
    Path(artist_name): Path<String>,
) -> Result<Json<Vec<AlbumResponse>>, (StatusCode, String)> {
    let user_pool = state
        .get_user_pool(&_claims.sub)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (raw_name, decoded_name) = artist_name_variants(&artist_name);
    let albums = sqlx::query_as::<_, AlbumResponse>(
        "SELECT DISTINCT id, name, artist, art_path
         FROM albums
         WHERE artist = ? OR artist = ?
            OR id IN (SELECT DISTINCT album_id FROM tracks WHERE artist = ? OR artist = ?)
         ORDER BY name",
    )
    .bind(raw_name)
    .bind(&decoded_name)
    .bind(raw_name)
    .bind(&decoded_name)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(albums))
}

pub async fn get_artist_tracks(
    _claims: Claims,
    State(state): State<AppState>,
    Path(artist_name): Path<String>,
) -> Result<Json<Vec<TrackResponse>>, (StatusCode, String)> {
    let user_pool = state
        .get_user_pool(&_claims.sub)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (raw_name, decoded_name) = artist_name_variants(&artist_name);
    let tracks = sqlx::query_as::<_, TrackResponse>(&track_select_sql(
        "WHERE artist = ? OR artist = ? ORDER BY album, disc_number, track_number, title",
    ))
    .bind(raw_name)
    .bind(&decoded_name)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_name_variants_decodes_plus_without_dropping_raw() {
        let (raw, decoded) = artist_name_variants("Artist+Name+Here");
        assert_eq!(raw, "Artist+Name+Here");
        assert_eq!(decoded, "Artist Name Here");

        let (raw_literal, decoded_literal) = artist_name_variants("Florence + The Machine");
        assert_eq!(raw_literal, "Florence + The Machine");
        assert_eq!(decoded_literal, "Florence   The Machine");
    }
}

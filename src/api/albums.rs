use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::Claims;
use crate::api::tracks::{track_select_sql, PaginatedQuery, TrackResponse};
use sqlx::Row;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct AlbumResponse {
    pub id: i64,
    pub name: String,
    pub artist: Option<String>,
    pub art_path: Option<String>,
}

pub async fn get_albums(
    _claims: Claims,
    State(state): State<AppState>,
    Query(query): Query<PaginatedQuery>,
) -> Result<Json<Vec<AlbumResponse>>, (StatusCode, String)> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(50).max(1);
    let offset = (page - 1) * limit;

    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let albums = sqlx::query_as::<_, AlbumResponse>(
        "SELECT id, name, artist, art_path FROM albums 
         ORDER BY artist, name
         LIMIT ? OFFSET ?"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(albums))
}

pub async fn get_album_by_id(
    _claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AlbumResponse>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let album = sqlx::query_as::<_, AlbumResponse>(
        "SELECT id, name, artist, art_path FROM albums WHERE id = ?"
    )
    .bind(id)
    .fetch_optional(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((StatusCode::NOT_FOUND, "Album not found".to_string()))?;

    Ok(Json(album))
}

pub async fn get_album_tracks(
    _claims: Claims,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<TrackResponse>>, (StatusCode, String)> {
    let user_pool = state.get_user_pool(&_claims.sub).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let tracks = sqlx::query_as::<_, TrackResponse>(
        &track_select_sql("WHERE album_id = ? ORDER BY disc_number, track_number, title")
    )
    .bind(id)
    .fetch_all(&user_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tracks))
}

pub async fn get_album_artwork(
    claims: Option<Claims>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let art_path = if let Some(ref c) = claims {
        state.find_artwork_path_for_user(&c.sub, id).await
    } else {
        None
    };

    let art_path = match art_path {
        Some(path) => Some(path),
        None => state.find_artwork_path(id).await,
    };

    if let Some(ref path) = art_path {
        let full_path = state.config.data_dir.join(path);
        if full_path.exists() {
            if let Ok(bytes) = std::fs::read(&full_path) {
                let mime = mime_guess::from_path(&full_path).first_or_octet_stream();
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime.to_string())],
                    bytes,
                ).into_response();
            }
        }
    }

    StatusCode::NOT_FOUND.into_response()
}

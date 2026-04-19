mod album;
mod background;
mod config;
mod dither;

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use album::AlbumClient;
use config::{Config, ScreenConfig};

#[derive(Clone)]
struct AppState {
    screens: Arc<HashMap<String, (ScreenConfig, AlbumClient)>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "immich_ink_frame=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".to_string());
    let config = Config::from_file(&config_path)?;
    tracing::info!(path = %config_path, screens = config.screens.len(), "loaded config");

    let screens: HashMap<String, (ScreenConfig, AlbumClient)> = config
        .screens
        .into_iter()
        .map(|s| {
            let client = AlbumClient::new(s.share_url.clone())?;
            Ok::<_, anyhow::Error>((s.name.clone(), (s, client)))
        })
        .collect::<anyhow::Result<_>>()?;

    let state = AppState { screens: Arc::new(screens) };

    let app = Router::new()
        .route("/screen/{name}", get(screen_handler))
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn screen_handler(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let (screen, client) = state
        .screens
        .get(&name)
        .ok_or_else(|| AppError::NotFound(format!("screen `{name}` not found")))?;

    tracing::info!(screen = %name, "fetching image");
    let img = client.random_frame(screen.width, screen.height, &screen.fit).await?;
    let img = background::apply(img, screen.width, screen.height, &screen.background)?;

    let png = tokio::task::spawn_blocking({
        let dither_cfg = screen.dither.clone();
        move || dither::process(img, &dither_cfg)
    })
    .await
    .map_err(|e| anyhow::anyhow!("dither task panicked: {e}"))??;

    let mut response = png.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    Ok(response)
}

// --- Error handling ---

enum AppError {
    NotFound(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        Self::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
            Self::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    }
}

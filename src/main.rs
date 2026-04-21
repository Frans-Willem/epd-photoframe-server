mod album;
mod background;
mod config;
mod dither;
mod infobox;
mod screen_state;
mod weather;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use chrono_tz::Tz;
use reqwest::Client;
use serde::Deserialize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use album::AlbumClient;
use config::{Config, ScreenConfig};
use screen_state::{ScreenState, resolve_index, resolve_tz, seconds_until};

struct Screen {
    config: ScreenConfig,
    album: AlbumClient,
    state: Mutex<ScreenState>,
    tz: Tz,
}

#[derive(Clone)]
struct AppState {
    screens: Arc<HashMap<String, Screen>>,
    http: Client,
}

#[derive(Debug, Deserialize)]
struct ScreenQuery {
    #[serde(default)]
    action: Option<Action>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Next,
    Previous,
    Refresh,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "epd_photoframe_server=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".to_string());
    let config = Config::from_file(&config_path)?;
    tracing::info!(path = %config_path, screens = config.screens.len(), "loaded config");

    let now = Utc::now();
    let screens: HashMap<String, Screen> = config
        .screens
        .into_iter()
        .map(|s| {
            let album = AlbumClient::new(s.share_url.clone())?;
            let tz = resolve_tz(s.timezone.as_deref())?;
            let state = Mutex::new(ScreenState::fresh(s.rotate.as_ref(), &tz, now));
            let screen = Screen { album, state, tz, config: s };
            Ok::<_, anyhow::Error>((screen.config.name.clone(), screen))
        })
        .collect::<anyhow::Result<_>>()?;

    let state = AppState {
        screens: Arc::new(screens),
        http: Client::builder().build()?,
    };

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
    Query(q): Query<ScreenQuery>,
    OriginalUri(uri): OriginalUri,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let screen = state
        .screens
        .get(&name)
        .ok_or_else(|| AppError::NotFound(format!("screen `{name}` not found")))?;

    let now = Utc::now();
    let (seed, cursor, next_rotation) = {
        let mut st = screen.state.lock().expect("screen state poisoned");
        st.maybe_rotate(screen.config.rotate.as_ref(), &screen.tz, now);
        match q.action {
            Some(Action::Next) => st.advance(1),
            Some(Action::Previous) => st.advance(-1),
            Some(Action::Refresh) | None => {}
        }
        (st.seed(), st.cursor(), st.next_rotation())
    };

    tracing::info!(screen = %name, ?q.action, seed, cursor, "fetching image");
    let cfg = &screen.config;
    let img = screen
        .album
        .pick(cfg.width, cfg.height, &cfg.fit, |n| {
            resolve_index(seed, cursor, n)
        })
        .await?;
    let mut img = background::apply(img, cfg.width, cfg.height, &cfg.background)?;

    if let Some(infobox_cfg) = &cfg.infobox {
        infobox::apply(&mut img, infobox_cfg, &screen.tz, &state.http).await?;
    }

    let png = tokio::task::spawn_blocking({
        let dither_cfg = cfg.dither.clone();
        move || dither::process(img, &dither_cfg)
    })
    .await
    .map_err(|e| anyhow::anyhow!("dither task panicked: {e}"))??;

    let mut response = png.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));

    // Tell the client when to come back; URL strips query params so a next/
    // previous action doesn't repeat on auto-refresh.
    if let Some(next) = next_rotation
        && let Ok(hv) = HeaderValue::from_str(&format!("{}; url={}", seconds_until(next, now), uri.path()))
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static("refresh"), hv);
    }

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

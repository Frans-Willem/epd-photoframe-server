mod album;
mod background;
mod color;
mod config;
mod dither;
mod infobox;
mod screen_state;
mod weather;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{NaiveTime, Utc};
use chrono_tz::Tz;
use reqwest::Client;
use serde::Deserialize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use album::AlbumClient;
use config::{Config, ScreenConfig};
use screen_state::{ScreenState, parse_rotate_at, resolve_index};

struct Screen {
    config: ScreenConfig,
    album: AlbumClient,
    state: Mutex<ScreenState>,
    rotate_at: Option<NaiveTime>,
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
            let rotate_at = s.rotate_at.as_deref().map(parse_rotate_at).transpose()?;
            let tz = resolve_system_tz()?;
            let screen = Screen {
                album,
                state: Mutex::new(ScreenState::fresh(now)),
                rotate_at,
                tz,
                config: s,
            };
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
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let screen = state
        .screens
        .get(&name)
        .ok_or_else(|| AppError::NotFound(format!("screen `{name}` not found")))?;

    let (seed, cursor) = {
        let mut st = screen.state.lock().expect("screen state poisoned");
        st.maybe_rotate(screen.rotate_at, &screen.tz, Utc::now());
        match q.action {
            Some(Action::Next) => st.advance(1),
            Some(Action::Previous) => st.advance(-1),
            Some(Action::Refresh) | None => {}
        }
        (st.seed(), st.cursor())
    };

    tracing::info!(screen = %name, ?q.action, seed, cursor, "fetching image");
    let cfg = &screen.config;
    let img = screen
        .album
        .pick(cfg.width, cfg.height, &cfg.fit, |n| {
            resolve_index(seed, cursor, n)
        })
        .await?;
    let img = background::apply(img, cfg.width, cfg.height, &cfg.background)?;

    let img = if let Some(infobox_cfg) = &cfg.infobox {
        let mut rgb = img.to_rgb8();
        infobox::apply(&mut rgb, infobox_cfg, &state.http).await?;
        image::DynamicImage::ImageRgb8(rgb)
    } else {
        img
    };

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
    Ok(response)
}

fn resolve_system_tz() -> anyhow::Result<Tz> {
    let name = iana_time_zone::get_timezone().context("detecting system timezone")?;
    name.parse::<Tz>()
        .map_err(|e| anyhow::anyhow!("unknown timezone `{name}`: {e}"))
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

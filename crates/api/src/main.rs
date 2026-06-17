//! wp-tt-dashboard read-only API (axum). Reads ONLY PostgreSQL (L1/L2) and, for the
//! live map, subscribes to the WP-TT GPS websocket via the local SSH tunnel. This crate
//! has NO Oracle/SSH access — it cannot reach production Oracle.

mod agg;
mod cycles;
mod db;
mod learn;
mod live;
mod livemap;
mod models;
mod periods;
mod routes;
mod workpool;

use std::sync::Arc;

use axum::extract::FromRef;
use axum::{routing::get, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

/// Combined app state. Existing handlers take `State<PgPool>`; the `FromRef` impls let
/// both that and `State<Arc<LiveMap>>` be extracted from this one state.
#[derive(Clone)]
struct AppState {
    pool: sqlx::PgPool,
    livemap: Arc<livemap::LiveMap>,
}

impl FromRef<AppState> for sqlx::PgPool {
    fn from_ref(s: &AppState) -> sqlx::PgPool {
        s.pool.clone()
    }
}
impl FromRef<AppState> for Arc<livemap::LiveMap> {
    fn from_ref(s: &AppState) -> Arc<livemap::LiveMap> {
        s.livemap.clone()
    }
}

fn app(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/kpis", get(routes::kpis))
        .route("/api/kpis/history", get(routes::kpi_history))
        .route("/api/kpis/:key/trend", get(routes::trend))
        .route("/api/breakdown/qc", get(routes::breakdown_qc))
        .route("/api/stats/:key", get(routes::stats))
        .route("/api/live", get(live::live))
        .route("/api/live/vessels", get(live::vessels))
        .route("/api/livemap/positions", get(livemap::positions))
        .route("/api/livemap/health", get(livemap::health))
        .route("/api/weather", get(livemap::weather))
        .route("/api/workpool", get(workpool::workpool))
        .route("/api/tt-cycles/summary", get(cycles::summary))
        .route("/api/tt-cycles/detail", get(cycles::detail))
        .route("/api/learn/topos", get(learn::topos))
        .route("/api/learn/lanes", get(learn::lanes))
        .route("/api/learn/travel", get(learn::travel))
        .route("/api/learn/soon-idle", get(learn::soon_idle))
        .route("/api/health", get(routes::health))
        .layer(CorsLayer::permissive()) // dev; tighten to the dashboard origin in prod
        .with_state(state);

    // Knowledge center — Astro Starlight static build at /kc/ (base '/kc'; dist is flat, so
    // nest_service strips '/kc' and ServeDir resolves dist/<path>/index.html). Built with
    // `cd docs-site && npm run build`. Reachable internally over Tailscale.
    // no-cache = always revalidate (cheap 304s): hashed _astro assets are immutable anyway.
    let kc_dir = std::env::var("KC_DIR").unwrap_or_else(|_| "docs-site/dist".to_string());
    let kc = tower::ServiceBuilder::new()
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        ))
        .service(ServeDir::new(&kc_dir));
    let api = api.nest_service("/kc", kc);

    // Serve the built SPA (if present) and fall back to index.html for client routing.
    let web_dist = std::env::var("WEB_DIST").unwrap_or_else(|_| "web/dist".to_string());
    let index = format!("{web_dist}/index.html");
    let spa = ServeDir::new(&web_dist).not_found_service(ServeFile::new(index));

    api.fallback_service(spa)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let pool = db::pool().await?;
    let livemap = livemap::LiveMap::new();
    livemap::load_centroids(&livemap, &pool).await; // restore learned topos coords before ingest
    livemap::load_lanes(&livemap, &pool).await; // restore learned driving-lane grid before ingest
    livemap::spawn(livemap.clone()); // background GPS ingest (via local SSH tunnel)
    livemap::spawn_util_sampler(livemap.clone(), pool.clone()); // 60s TT-utilization samples
    livemap::spawn_assignment_refresh(livemap.clone(), pool.clone()); // 30s work-pool assignment cache
    livemap::spawn_cycle_flusher(livemap.clone(), pool.clone()); // 30s persist completed TT cycles
    livemap::spawn_learn_persist(livemap.clone(), pool.clone()); // 5min persist learned topos coords + lanes + quality
    livemap::spawn_travel_aggregator(pool.clone()); // 5min harvest TT travel-time labels from cycles
    livemap::spawn_density_sampler(livemap.clone(), pool.clone()); // 60s per-cell TT density (4 grid sizes)
    livemap::spawn_soon_idle_logger(livemap.clone(), pool.clone()); // 30s soon_idle 예측 적재(그림자 정확도)
    let state = AppState { pool, livemap };

    let addr = std::env::var("API_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "wp-api listening");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

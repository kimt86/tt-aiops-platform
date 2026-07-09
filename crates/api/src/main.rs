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
mod roadgraph;
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
        .route("/api/livemap/wharf", get(livemap::wharf))
        .route("/api/livemap/health", get(livemap::health))
        .route("/api/weather", get(livemap::weather))
        .route("/api/workpool", get(workpool::workpool))
        .route("/api/stage2/shadow", get(workpool::stage2_shadow))
        .route("/api/stage2/advisory", get(workpool::stage2_advisory))
        .route("/api/stage2/compare", get(workpool::dispatch_compare))
        .route("/api/stage2/fair-compare", get(workpool::stage2_fair_compare))
        .route("/api/stage2/fair-breakdown", get(workpool::stage2_fair_breakdown))
        .route("/api/stage2/compare-picks", get(workpool::stage2_compare_picks))
        .route("/api/stage2/work-points", get(workpool::stage2_work_points))
        .route("/api/health/dispatch", get(workpool::health_dispatch))
        .route("/api/tt-cycles/summary", get(cycles::summary))
        .route("/api/tt-cycles/detail", get(cycles::detail))
        .route("/api/learn/topos", get(learn::topos))
        .route("/api/learn/lanes", get(learn::lanes))
        .route("/api/learn/travel", get(learn::travel))
        .route("/api/learn/soon-idle", get(learn::soon_idle))
        .route("/api/learn/dispatch-pred", get(learn::dispatch_pred))
        .route("/api/learn/extra", get(learn::extra))
        .route("/api/learn/data-catalog", get(learn::data_catalog))
        .route("/api/learn/data-sample", get(learn::data_sample))
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
    // [retired mig 0081] spawn_leg_decomp (empty-leg drive/stop decomp) removed; cost now from learn_travel_sample empty trips (mig 0080).
    roadgraph::spawn_roadgraph_eval(pool.clone()); // 10min: route empty trips vs 순수주행 label → road_route_eval (gate metric + RouteCost calibration)
    livemap::spawn_density_sampler(livemap.clone(), pool.clone()); // 60s per-cell TT density (4 grid sizes)
    livemap::spawn_soon_idle_logger(livemap.clone(), pool.clone()); // 30s soon_idle 예측 적재(그림자 정확도)
    livemap::spawn_free_in_logger(livemap.clone(), pool.clone()); // 60s free_in 학습+검증셋(스냅샷+실제잔여 backfill)
    livemap::spawn_qc_wait_logger(livemap.clone(), pool.clone()); // 30s QC starvation 적재(K_QC_TT_WAIT_GPS, topos vs GPS 비교)
    livemap::spawn_qc_wait_kpi(pool.clone()); // 5min: qc_wait_sample → kpi_daily/shift (K_QC_TT_WAIT_GPS 영속)
    workpool::spawn_dispatch_pred_logger(pool.clone()); // 2min: 배차 1단계 예측 검증 로그(dispatch_pred_sample)
    livemap::spawn_qc_handover_logger(livemap.clone(), pool.clone()); // 10s: LD 핸드오버 엣지 섀도(mig0087, 탐지 검증)
    livemap::spawn_selfcal_refresh(livemap.clone(), pool.clone()); // 15min: ⑤곧빔게이트·⑥유휴분 잔차 자가보정(mig0084)
    livemap::spawn_stage2_shadow(livemap.clone(), pool.clone()); // 60s: Stage-2 매칭 그림자(stage2_match_shadow)
    livemap::spawn_mapmatch_shadow(livemap.clone(), pool.clone()); // 5s: 도로망 맵매칭 그림자(mm_arrival_shadow, 도착 포착 개선 측정)
    livemap::spawn_pos_hist(livemap.clone(), pool.clone()); // 30s: 트럭 위치·상태 이력(truck_pos_hist)
    livemap::spawn_pos_hist_hifreq(livemap.clone(), pool.clone()); // 3s: 도로망 추론용 고빈도 GPS(truck_pos_hifreq)
    livemap::spawn_rtg_pos_hist(livemap.clone(), pool.clone()); // 3s: RTG/ES GPS 이력(rtg_pos_hist) — 핸드오버 포착 연구용
    livemap::spawn_cycle_pickup_correct(pool.clone()); // 5m: 픽업완료(③) TOS 크레인 정답지 보정(pickup_done_at, mig0088)
    livemap::spawn_dispatch_compare(livemap.clone(), pool.clone()); // 60s: TOS vs 우리 배차 비교(dispatch_compare_shadow)
    livemap::spawn_fair_compare(livemap.clone(), pool.clone()); // 5min: 공정 1:1 최적매칭 vs TOS(fair_compare_shadow)
    let state = AppState { pool, livemap };

    let addr = std::env::var("API_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "wp-api listening");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

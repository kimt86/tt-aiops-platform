//! Hourly terminal weather from Open-Meteo (free, no API key) → weather_hourly. A travel-time
//! feature: precipitation / high wind / low visibility slow yard driving. No Oracle — shells out
//! to `curl` like the ETW gateway fetch (workpool::src_etw). Run hourly (wp-weather.timer);
//! requests past_days=1 + forecast so brief gaps self-heal on the next tick. Stored in UTC hour
//! buckets, joined to travel samples by date_trunc('hour', dropped_at). See research/travel-time.
use anyhow::{Context, Result};
use sqlx::PgPool;

use crate::kpis::common::run_logged;

// Port Klang / Westports yard centroid (avg of learned block coords).
const LAT: f64 = 2.9252;
const LON: f64 = 101.2927;

/// One hourly poll: fetch the recent + near-term hourly weather and upsert it. Idempotent (PK ts).
pub async fn tick_weather(pool: &PgPool) -> Result<()> {
    let run_date = wp_core::shift::terminal_now().date_naive();
    run_logged(pool, "WEATHER", run_date, |_| async move {
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={LAT}&longitude={LON}\
             &hourly=precipitation,wind_speed_10m,visibility,weather_code\
             &past_days=1&forecast_days=1&timezone=GMT"
        );
        // Match the codebase HTTP idiom: shell out to curl (the api crate has no HTTP client and
        // the extractor already fetches the ETW gateway this way). GMT == UTC for storage.
        let out = tokio::process::Command::new("curl")
            .args(["-fsS", "-m", "15", &url])
            .output()
            .await
            .context("curl open-meteo")?;
        anyhow::ensure!(out.status.success(), "open-meteo fetch failed (curl status)");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("parse open-meteo json")?;
        let h = v.get("hourly").context("open-meteo: no hourly block")?;
        let times = h.get("time").and_then(|x| x.as_array()).context("open-meteo: no time array")?;
        let col = |k: &str| h.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let (precip, wind, vis, code) = (
            col("precipitation"),
            col("wind_speed_10m"),
            col("visibility"),
            col("weather_code"),
        );

        let mut tx = pool.begin().await?;
        let mut n = 0u64;
        for (i, t) in times.iter().enumerate() {
            let Some(ts_str) = t.as_str() else { continue };
            // Open-Meteo GMT time "YYYY-MM-DDTHH:MM" → UTC instant.
            let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M") else { continue };
            let ts = ndt.and_utc();
            let at = |a: &Vec<serde_json::Value>| a.get(i).and_then(|x| x.as_f64());
            sqlx::query(
                "INSERT INTO weather_hourly (ts, precip_mm, wind_kmh, visibility_m, weather_code)
                 VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (ts) DO UPDATE SET
                   precip_mm = EXCLUDED.precip_mm, wind_kmh = EXCLUDED.wind_kmh,
                   visibility_m = EXCLUDED.visibility_m, weather_code = EXCLUDED.weather_code,
                   captured_at = now()",
            )
            .bind(ts)
            .bind(at(&precip))
            .bind(at(&wind))
            .bind(at(&vis))
            .bind(at(&code).map(|x| x as i32))
            .execute(&mut *tx)
            .await?;
            n += 1;
        }
        tx.commit().await?;
        Ok(n)
    })
    .await?;
    Ok(())
}

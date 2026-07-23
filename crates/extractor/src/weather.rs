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
    let run_date = tt_core::shift::terminal_now().date_naive();
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

/// Live 1-minute weather from Tomorrow.io (free, NO credit card; 500/day·25/hr). One POST to
/// /v4/timelines (timestep 1m over a short past window) returns a 1-minute series, so polling
/// ~every 3 min (≈480/day, 20/hr — under both caps) gives full 1-minute coverage AND a fresh
/// value for the live map. Targets intermittent squalls that hourly/15-min averages miss. Needs
/// env TOMORROW_API_KEY. Model nowcast (not a gauge). Idempotent upsert. See research/travel-time.
pub async fn tick_weather_live(pool: &PgPool) -> Result<()> {
    let run_date = tt_core::shift::terminal_now().date_naive();
    run_logged(pool, "WEATHER_1MIN", run_date, |_| async move {
        let key = std::env::var("TOMORROW_API_KEY").context("TOMORROW_API_KEY not set")?;
        let url = format!("https://api.tomorrow.io/v4/timelines?apikey={key}");
        // short past window at 1-min resolution; overlap heals missed polls (upsert dedups).
        let body = format!(
            r#"{{"location":[{LAT},{LON}],"fields":["precipitationIntensity","visibility","windSpeed","weatherCode"],"timesteps":["1m"],"startTime":"nowMinus10m","endTime":"now","units":"metric","timezone":"UTC"}}"#
        );
        let out = tokio::process::Command::new("curl")
            .args(["-fsS", "-m", "15", "-X", "POST", "-H", "Content-Type: application/json", "-d", &body, &url])
            .output()
            .await
            .context("curl tomorrow.io")?;
        anyhow::ensure!(out.status.success(), "tomorrow.io fetch failed (curl status / key / quota)");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("parse tomorrow.io json")?;
        let intervals = v
            .pointer("/data/timelines/0/intervals")
            .and_then(|x| x.as_array())
            .context("tomorrow.io: no intervals (check key/quota)")?;
        let mut tx = pool.begin().await?;
        let mut n = 0u64;
        for it in intervals {
            let Some(ts_str) = it.get("startTime").and_then(|x| x.as_str()) else { continue };
            let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str) else { continue };
            let val = |k: &str| it.pointer("/values").and_then(|vv| vv.get(k)).and_then(|x| x.as_f64());
            sqlx::query(
                "INSERT INTO weather_1min (ts, precip_mm_hr, visibility_km, wind_ms, weather_code)
                 VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (ts) DO UPDATE SET
                   precip_mm_hr = EXCLUDED.precip_mm_hr, visibility_km = EXCLUDED.visibility_km,
                   wind_ms = EXCLUDED.wind_ms, weather_code = EXCLUDED.weather_code, captured_at = now()",
            )
            .bind(ts.with_timezone(&chrono::Utc))
            .bind(val("precipitationIntensity"))
            .bind(val("visibility"))
            .bind(val("windSpeed"))
            .bind(val("weatherCode").map(|x| x as i32))
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

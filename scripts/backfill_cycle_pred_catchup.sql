-- 1회용 소급 백필: cycle_pred_shadow 의 오래된 공백을 메운다.
--
--   사용: PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt \
--           -v days=12 -f scripts/backfill_cycle_pred_catchup.sql
--
-- 왜 필요한가:
--   상시 백필(scripts/populate_cycle_pred_shadow.sql)은 `WHERE free_ts >= now() - interval
--   '6 hours'` 로 최근 6시간만 본다. 정상 운영에선 충분하지만, 백필이 한 번 멈추면 그 구간은
--   **영영 못 채운다**. 실제로 2026-07-25~26 에 스키마 불일치로 백필이 이틀 멈췄고(mig0104 를
--   적용하지 않고 코드를 머지한 사고), 07-26 은 지금도 채움률 11% 다(미백필 14,091행).
--   원천 tt_move_log 는 아직 그 날짜를 들고 있으므로(07-26 23,822행) 회수 가능하다.
--
-- 로직은 상시 백필과 **완전히 동일**하고 창만 넓힌다 — 그래야 소급분과 평시분이 같은 규칙으로
-- 채워진다. 다른 규칙으로 메우면 나중에 정확도를 잴 때 두 모집단이 섞인다.
--
-- 안전: UPDATE 대상은 actual_free_at IS NULL 인 행뿐이라 이미 채워진 값은 건드리지 않는다.
-- 멱등: 다시 돌려도 이미 채워진 행은 대상에서 빠진다.

\set days :days
\echo '--- 실행 전 채움률 ---'
SELECT captured_at::date AS 일자, count(*) AS 행,
       round(100.0 * count(*) FILTER (WHERE actual_free_at IS NOT NULL) / count(*)) || '%' AS 채움률
  FROM cycle_pred_shadow
 WHERE captured_at > now() - make_interval(days => :days)
 GROUP BY 1 ORDER BY 1;

WITH trips AS (
  SELECT ytno, min(pickup_ts) AS pk, max(free_ts) AS fr,
         least(coalesce(max(twin_group_size), 1), 2) AS nc
  FROM tt_move_log
  WHERE free_ts >= now() - make_interval(days => :days)   -- ← 상시 백필의 '6 hours' 를 넓힌 유일한 차이
  GROUP BY ytno, dispatch_ts
),
best AS (
  SELECT DISTINCT ON (s.ytno, s.pickup_ts) s.ytno, s.pickup_ts, t.fr, t.nc
  FROM cycle_pred_shadow s
  JOIN trips t ON t.ytno = s.ytno
    AND s.pickup_ts >= t.pk - interval '60 s'
    AND s.pickup_ts <= t.pk + interval '5 min'
  WHERE s.actual_free_at IS NULL
  ORDER BY s.ytno, s.pickup_ts, abs(extract(epoch FROM s.pickup_ts - t.pk))
)
UPDATE cycle_pred_shadow s
SET actual_free_at     = b.fr,
    actual_remaining_s = extract(epoch FROM b.fr - s.pickup_ts)::int,
    n_containers       = b.nc,
    pred_remaining_s   = lr.remaining_p50,
    pred_free_at       = s.pickup_ts + make_interval(secs => lr.remaining_p50),
    err_s              = lr.remaining_p50 - extract(epoch FROM b.fr - s.pickup_ts)::int
FROM best b
JOIN learn_cycle_remaining lr
  ON lr.dest_inflight_bucket = -1 AND lr.n_containers = b.nc
WHERE s.ytno = b.ytno AND s.pickup_ts = b.pickup_ts AND lr.jobtype = s.jobtype;

-- 트윈 다리 정리(상시 백필과 동일): 같은 free 로 채워진 뒤쪽 다리를 지우고 첫 픽업만 남긴다
DELETE FROM cycle_pred_shadow s USING (
  SELECT ytno, actual_free_at, min(pickup_ts) AS keep
  FROM cycle_pred_shadow WHERE actual_free_at IS NOT NULL
  GROUP BY ytno, actual_free_at
) d
WHERE s.ytno = d.ytno AND s.actual_free_at = d.actual_free_at AND s.pickup_ts <> d.keep;

\echo '--- 실행 후 채움률 ---'
SELECT captured_at::date AS 일자, count(*) AS 행,
       round(100.0 * count(*) FILTER (WHERE actual_free_at IS NOT NULL) / count(*)) || '%' AS 채움률
  FROM cycle_pred_shadow
 WHERE captured_at > now() - make_interval(days => :days)
 GROUP BY 1 ORDER BY 1;

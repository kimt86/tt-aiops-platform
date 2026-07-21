-- 0099: tt_cycle_recon 에 트립의 전체 컨테이너 ID 배열(contnos)을 추가.
-- 대표 ID(contno)는 그대로 두고, contnos[]에 그 트립의 모든 컨테이너 ID를 free_ts 순으로 담는다.
--   단일 트립 = 1개, 트윈 = 2개. contno = contnos[1] (대표).
-- 불변식: array_length(contnos, 1) = n_containers 항상 성립 — tt_move_log PK가 contno를 포함하므로
--   한 트립(ytno,dispatch_ts) 안에서 contno는 유일(중복 없음). DISTINCT를 쓰지 말 것(개수 desync).
-- 백필: 기존 행은 NULL로 남고, populate 재실행 시 ON CONFLICT DO UPDATE ... WHERE contnos IS NULL 로
--   contnos만 채운다(GPS 분해 컬럼은 건드리지 않음 → "freshest GPS wins" 불변식 유지).
ALTER TABLE tt_cycle_recon ADD COLUMN IF NOT EXISTS contnos text[];
COMMENT ON COLUMN tt_cycle_recon.contnos IS
  '트립의 모든 컨테이너 ID(free_ts 순). 단일 1개·트윈 2개. contno = contnos[1] 대표. len=n_containers.';

-- One-time UNBOUNDED backfill (identity-only, GPS-independent) so EVERY existing recon row — including rows
-- older than the recurring populate window (read window is up to 14 days) — gets contnos. Without this, aged
-- pre-0099 twins would keep contnos=NULL and render as ×N with a single ID. Touches ONLY container-identity
-- columns (never GPS/timing). Idempotent: only rows whose stored identity differs are updated → re-running is safe.
WITH ident AS (
  SELECT ytno, dispatch_ts,
         array_agg(contno ORDER BY free_ts, contno)      AS contnos,
         (array_agg(contno ORDER BY free_ts, contno))[1] AS contno,
         count(*)::int                                   AS n_containers,
         bool_or(coalesce(is_twin,false))                AS is_twin
  FROM tt_move_log GROUP BY ytno, dispatch_ts
)
UPDATE tt_cycle_recon r SET
  contnos = i.contnos, contno = i.contno, n_containers = i.n_containers, is_twin = i.is_twin
FROM ident i
WHERE r.ytno = i.ytno AND r.dispatch_ts = i.dispatch_ts
  AND (r.contnos IS DISTINCT FROM i.contnos OR r.n_containers <> i.n_containers OR r.is_twin <> i.is_twin);

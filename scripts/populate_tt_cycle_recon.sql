-- Populate / refresh tt_cycle_recon by joining tt_move_log (TOS cycle boundaries) with
-- truck_pos_hifreq (raw GPS positions). No Oracle load — pure Postgres join.
--
-- Method (validated + adversarially reviewed): for each physical trip (tt_move_log twin_leg_seq=1),
-- take raw fixes in [dispatch_ts, free_ts], split by pickup_ts into empty/laden legs, and segment each
-- leg's consecutive fixes into DRIVE vs STOP. lag() is partitioned PER LEG so no segment crosses pickup.
-- Per inter-fix segment (dt = gap seconds, dist_m = haversine):
--   * well-sampled (dt<=60s) & real move (dist>=12m & >=1.0 m/s)   -> drive = dt        (rolling)
--   * long silent gap (dt>60s) that relocated (dist>=100m)          -> drive = LEAST(dt, dist/5.5)
--                                                                     (only nominal transit is driving;
--                                                                      GPS is silent WHEN STOPPED, so the
--                                                                      rest of a long gap is parking -> stop)
--   * otherwise                                                     -> stop
-- edge_wait_s = cycle_s − observed absorbs the silent boundary dwell and is split into
-- dispatch_wait / pickup_dwell / drop_dwell. Reconciles: drive+stop+edge_wait = cycle_s exactly.
--
-- Idempotent: ON CONFLICT (ytno, dispatch_ts) DO NOTHING → first (freshest-GPS) computation wins.
-- truck_pos_hifreq is retained ~1 day, so run the timer well under that (:days small) or cycles land
-- boundary-only (gps_covered=false, edge_wait=cycle_s). Window parameterized by :days (default 2).

\if :{?days}
\else
  \set days 2
\endif

WITH cyc AS (
  -- One physical trip per (ytno, dispatch_ts): collapse twin/tandem legs. pickup = first loaded (MIN),
  -- free = last freed (MAX) so a DS twin's staggered second drop (~2min later) is inside the cycle.
  SELECT ytno, dispatch_ts,
         min(pickup_ts)                                             AS pickup_ts,
         max(free_ts)                                               AS free_ts,
         round(EXTRACT(EPOCH FROM max(free_ts) - dispatch_ts))::int AS cycle_s,
         (array_agg(jobtype ORDER BY free_ts))[1]                   AS jobtype,
         (array_agg(contno  ORDER BY free_ts, contno))[1]           AS contno,
         array_agg(contno ORDER BY free_ts, contno)                 AS contnos,   -- contno tiebreak = deterministic; contno = contnos[1] always (LD twins share free_ts)
         bool_or(coalesce(is_twin,false))                          AS is_twin,
         count(*)::int                                              AS n_containers,
         (max(free_ts) AT TIME ZONE 'Asia/Kuala_Lumpur')::date     AS business_date,
         CASE WHEN EXTRACT(HOUR FROM (max(free_ts) AT TIME ZONE 'Asia/Kuala_Lumpur')) BETWEEN 6 AND 17
              THEN 'D' ELSE 'N' END                                 AS shift
  FROM tt_move_log
  WHERE free_ts >= now() - make_interval(days => :days)
  GROUP BY ytno, dispatch_ts
),
fx AS (
  SELECT c.ytno, c.dispatch_ts,
         (CASE WHEN p.ts <= c.pickup_ts THEN 'empty' ELSE 'laden' END) AS leg,
         p.ts, p.lat, p.lon
  FROM cyc c
  JOIN truck_pos_hifreq p ON p.ytno = c.ytno AND p.ts BETWEEN c.dispatch_ts AND c.free_ts
),
d AS (
  SELECT ytno, dispatch_ts, leg,
         EXTRACT(EPOCH FROM ts - lag(ts) OVER w) AS dt,
         2*6371000*asin(sqrt( power(sin(radians(lat-lag(lat) OVER w)/2),2)
             + cos(radians(lag(lat) OVER w))*cos(radians(lat))
             * power(sin(radians(lon-lag(lon) OVER w)/2),2) )) AS dist_m
  FROM fx
  WINDOW w AS (PARTITION BY ytno, dispatch_ts, leg ORDER BY ts)
),
seg AS (
  SELECT ytno, dispatch_ts, leg, dt, dist_m,
    CASE
      WHEN dt <= 60 AND dist_m >= 12 AND dist_m/dt >= 1.0 THEN dt          -- well-sampled rolling move
      WHEN dt > 60  AND dist_m >= 100 THEN LEAST(dt, dist_m/5.5)           -- long gap-bridge: nominal transit only
      ELSE 0
    END AS drv_s,
    (dt > 60 AND dist_m >= 100) AS long_gap,
    ((dt <= 60 AND dist_m >= 12 AND dist_m/dt >= 1.0) OR (dt > 60 AND dist_m >= 100)) AS is_move
  FROM d WHERE dt IS NOT NULL AND dt > 0
),
agg AS (
  SELECT ytno, dispatch_ts,
    round(coalesce(sum(drv_s)      FILTER (WHERE leg='empty'), 0))::int             e_drive_s,
    round(coalesce(sum(dt - drv_s) FILTER (WHERE leg='empty'), 0))::int             e_stop_s,
    round(coalesce(sum(dist_m)     FILTER (WHERE leg='empty' AND is_move), 0))::int e_drive_m,
    round(coalesce(sum(drv_s)      FILTER (WHERE leg='laden'), 0))::int             l_drive_s,
    round(coalesce(sum(dt - drv_s) FILTER (WHERE leg='laden'), 0))::int             l_stop_s,
    round(coalesce(sum(dist_m)     FILTER (WHERE leg='laden' AND is_move), 0))::int l_drive_m,
    round(coalesce(sum(dt)         FILTER (WHERE long_gap), 0))::int                long_gap_s
  FROM seg GROUP BY ytno, dispatch_ts
),
span AS (   -- leg fix boundaries for the edge-wait split
  SELECT ytno, dispatch_ts,
    count(*)::int n_fix,
    min(ts) FILTER (WHERE leg='empty') e_first, max(ts) FILTER (WHERE leg='empty') e_last,
    min(ts) FILTER (WHERE leg='laden') l_first, max(ts) FILTER (WHERE leg='laden') l_last
  FROM fx GROUP BY 1,2
)
INSERT INTO tt_cycle_recon
  (ytno, dispatch_ts, contno, contnos, jobtype, is_twin, n_containers, pickup_ts, free_ts, cycle_s,
   e_drive_s, e_stop_s, e_drive_m, l_drive_s, l_stop_s, l_drive_m,
   edge_wait_s, dispatch_wait_s, pickup_dwell_s, drop_dwell_s,
   gps_covered, n_fix, long_gap_s, business_date, shift)
SELECT c.ytno, c.dispatch_ts, c.contno, c.contnos, c.jobtype, c.is_twin, c.n_containers, c.pickup_ts, c.free_ts, c.cycle_s,
   coalesce(a.e_drive_s,0), coalesce(a.e_stop_s,0), coalesce(a.e_drive_m,0),
   coalesce(a.l_drive_s,0), coalesce(a.l_stop_s,0), coalesce(a.l_drive_m,0),
   c.cycle_s - coalesce(a.e_drive_s + a.e_stop_s + a.l_drive_s + a.l_stop_s, 0)               AS edge_wait_s,
   round(EXTRACT(EPOCH FROM coalesce(s.e_first, s.l_first, c.free_ts) - c.dispatch_ts))::int   AS dispatch_wait_s,
   CASE WHEN s.e_last IS NOT NULL AND s.l_first IS NOT NULL
        THEN round(EXTRACT(EPOCH FROM s.l_first - s.e_last))::int ELSE 0 END                    AS pickup_dwell_s,
   round(EXTRACT(EPOCH FROM c.free_ts - coalesce(s.l_last, s.e_last, c.free_ts)))::int          AS drop_dwell_s,
   (coalesce(a.e_drive_s,0) + coalesce(a.l_drive_s,0) > 0)                                      AS gps_covered,
   coalesce(s.n_fix,0), coalesce(a.long_gap_s,0), c.business_date, c.shift
FROM cyc c
LEFT JOIN agg  a USING (ytno, dispatch_ts)
LEFT JOIN span s USING (ytno, dispatch_ts)
-- Freshest-GPS-wins: the INSERT NEVER overwrites an existing row's GPS decomposition. New trips insert;
-- existing rows are left untouched here.
ON CONFLICT (ytno, dispatch_ts) DO NOTHING;

-- Container-identity refresh (GPS-INDEPENDENT, windowed). Backfills pre-0099 contnos, absorbs late-completed
-- twins (2nd drop after the first run), and picks up tt_move_log leg-set corrections (relabel/cancel).
-- Touches ONLY contno/contnos/n_containers/is_twin — never any GPS/timing/decomposition column — so it can
-- never clobber a good decomposition. Idempotent: only rows whose stored identity differs are updated (no churn).
-- Note: a late twin's cycle_s/GPS split stay frozen at first-drop (pre-existing DO-NOTHING behavior); only the
-- displayed container IDs/count become complete. Invariants hold: array_length(contnos)=n_containers, contno=contnos[1].
WITH ident AS (
  SELECT ytno, dispatch_ts,
         array_agg(contno ORDER BY free_ts, contno)      AS contnos,
         (array_agg(contno ORDER BY free_ts, contno))[1] AS contno,
         count(*)::int                                   AS n_containers,
         bool_or(coalesce(is_twin,false))                AS is_twin
  FROM tt_move_log
  WHERE dispatch_ts >= now() - make_interval(days => :days)   -- window on the GROUP-SHARED key: a trip's legs
  GROUP BY ytno, dispatch_ts                                   -- share dispatch_ts, so a twin is fully in or fully
)                                                              -- out — the boundary can never bisect it (else the
                                                               -- surviving leg would shrink the row 2→1 forever).
UPDATE tt_cycle_recon r SET
  contnos = i.contnos, contno = i.contno, n_containers = i.n_containers, is_twin = i.is_twin
FROM ident i
WHERE r.ytno = i.ytno AND r.dispatch_ts = i.dispatch_ts
  AND (r.contnos IS DISTINCT FROM i.contnos OR r.n_containers <> i.n_containers OR r.is_twin <> i.is_twin);

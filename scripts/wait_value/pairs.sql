-- 기다림의 값어치 — (트럭 자유 → 그 트럭의 다음 픽업) 쌍과 두 끝의 좌표를 뽑는다 (2026-10-08).
-- 사용: psql -At -F $'\t' -v from="'…'" -v to="'…'" -f scripts/wait_value/pairs.sql > pairs.tsv
--       python3 scripts/wait_value/analyze.py pairs.tsv
--
-- 분모: 창 안의 자유 사건(적하 = QC 내림 qc_move_log LD · 양하 = 야드 인계 tos_handover_label DS, 트윈은 마지막
--   하나) 중, 30분 안에 같은 트럭의 다음 픽업(양하 = QC 가 실어줌 qc_move_log DS · 적하 = 야드 장비가 실어줌
--   rtg_move_log LD)이 다음 자유보다 먼저 있고, 두 끝의 좌표가 다 잡힌 것.
-- 좌표: QC 끝 = learn_topos_point(크레인, 퍼짐 ~15m) · 양하 내림 = 인계 목적지(블록-베이, 없으면 블록) ·
--   적하 픽업 = 그 시각 직전 600초 안 마지막 트럭 GPS(정지하면 단말이 침묵하므로 마지막 픽스 ≈ 정지 위치)(야드 장비 위치 코드가 로컬에 없다). truck_pos_hist 는 2일 보관.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '300s';
WITH raw AS (
  SELECT trk_id AS ytno, comp_ts AS f, 'LD'::text AS fjt, machno AS floc FROM qc_move_log
   WHERE jobtype = 'LD' AND trk_id LIKE 'TT%' AND comp_ts >= :from AND comp_ts < :to
  UNION ALL
  SELECT ytno, comp_ts, 'DS', topos FROM tos_handover_label
   WHERE jobtype = 'DS' AND ytno LIKE 'TT%' AND comp_ts >= :from AND comp_ts < :to
), ev AS (
  SELECT *, lead(f) OVER (PARTITION BY ytno ORDER BY f) AS next_f FROM raw
), fr AS (
  SELECT * FROM ev WHERE next_f IS NULL OR next_f > f + interval '180 seconds'
), pk AS (
  SELECT trk_id AS ytno, comp_ts AS p, 'DS'::text AS pjt, machno AS ploc FROM qc_move_log
   WHERE jobtype = 'DS' AND trk_id LIKE 'TT%' AND comp_ts >= :from AND comp_ts < (:to)::timestamptz + interval '30 minutes'
  UNION ALL
  SELECT trk_id, comp_ts, 'LD', NULL FROM rtg_move_log
   WHERE jobtype = 'LD' AND trk_id LIKE 'TT%' AND comp_ts >= :from AND comp_ts < (:to)::timestamptz + interval '30 minutes'
), pair AS (
  SELECT fr.ytno, fr.f, fr.fjt, fr.floc, n.p, n.pjt, n.ploc
    FROM fr
    JOIN LATERAL (SELECT p, pjt, ploc FROM pk
                   WHERE pk.ytno = fr.ytno AND pk.p > fr.f AND pk.p < fr.f + interval '30 minutes'
                     AND (fr.next_f IS NULL OR pk.p < fr.next_f)
                   ORDER BY pk.p LIMIT 1) n ON true
)
SELECT pr.ytno, extract(epoch FROM pr.f)::bigint, pr.fjt, extract(epoch FROM pr.p)::bigint, pr.pjt,
       coalesce(fa.lat, fb.lat), coalesce(fa.lon, fb.lon),
       coalesce(pa.lat, g.lat), coalesce(pa.lon, g.lon)
  FROM pair pr
  LEFT JOIN learn_topos_point fa ON fa.topos = pr.floc
  LEFT JOIN learn_topos_point fb ON pr.fjt = 'DS' AND fb.topos = left(pr.floc, 3)
  LEFT JOIN learn_topos_point pa ON pr.pjt = 'DS' AND pa.topos = pr.ploc
  LEFT JOIN LATERAL (SELECT lat, lon FROM truck_pos_hist h
                      WHERE pr.pjt = 'LD' AND h.ytno = pr.ytno
                        AND h.ts <= pr.p AND h.ts > pr.p - interval '600 seconds'
                      ORDER BY h.ts DESC LIMIT 1) g ON true;
ROLLBACK;

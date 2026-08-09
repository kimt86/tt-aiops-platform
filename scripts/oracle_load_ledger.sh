#!/usr/bin/env bash
# oracle_load_ledger.sh — Oracle 부하 장부 (PLAN-extractor.md 1-1)
#
# 최근 24h를 systemd 유닛별 실행수/벽시계와 scengen gen_run 집계로 표로 출력하고
# data/oracle_load/ledger-<timestamp>.txt 에 저장한다.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Oracle 을 실제로 치는 유닛만 (2026-08-10 정비: scenario-collect/yard 는 CHUNK4 로컬화로 제외,
# vessel-schedule/stowplan-recon 추가. shift-t1 은 로컬이지만 벽시계 감시용으로 유지)
UNITS=(
  tt-qc-moves
  tt-rtg-moves
  tt-handover
  tt-workpool
  tt-vessel-schedule
  tt-shift-t1
  tt-shift-t2
  tt-stowplan
  tt-stowplan-recon
  tt-nightly
  tt-scenario-gate
  tt-scenario-contspec
  tt-scenario-enrich
)

OUT_DIR="data/oracle_load"
mkdir -p "$OUT_DIR"
STAMP="$(date +%Y%m%d-%H%M)"
OUT_FILE="$OUT_DIR/ledger-${STAMP}.txt"

{
  echo "# Oracle 부하 장부 — $(date -Iseconds)"
  echo "# 대상 유닛(최근 24h): ${UNITS[*]}"
  echo
  printf '%-24s %8s %12s %10s %10s\n' "unit" "runs" "wall_sum_s" "avg_s" "max_s"
  printf '%-24s %8s %12s %10s %10s\n' "----" "----" "----------" "-----" "-----"

  for u in "${UNITS[@]}"; do
    durations="$(journalctl --user -u "$u" --since '24 hours ago' -o short-unix 2>/dev/null \
      | awk '/Starting/{s=$1} /Finished/{if(s){print $1-s; s=""}}')"
    if [ -z "$durations" ]; then
      printf '%-24s %8d %12s %10s %10s\n' "$u" 0 "-" "-" "-"
      continue
    fi
    n=$(printf '%s\n' "$durations" | wc -l)
    sum=$(printf '%s\n' "$durations" | awk '{s+=$1} END{printf "%.1f", s}')
    max=$(printf '%s\n' "$durations" | sort -n | tail -1)
    avg=$(awk -v s="$sum" -v n="$n" 'BEGIN{printf "%.2f", s/n}')
    printf '%-24s %8d %12s %10s %10s\n' "$u" "$n" "$sum" "$avg" "$max"
  done

  echo
  echo "# scengen (scenario.gen_run, 최근 24h) — kind, runs, sum(query_ms)"
  source .env
  psql "$DATABASE_URL" -At -F' | ' -c \
    "SELECT kind, count(*), COALESCE(sum((load_stats->>'query_ms')::int),0) FROM scenario.gen_run WHERE started_at > now()-interval '24 hours' GROUP BY 1 ORDER BY 1;" \
    2>&1 || echo "(scenario.gen_run 조회 실패)"

} | tee "$OUT_FILE"

echo
echo "저장: $OUT_FILE"

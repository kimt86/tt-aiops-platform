#!/bin/bash
# TOS 배차 가능 목록(getMoverList) 2초 간격 실측 — tos_avail_interval 복원의 정답지 (2026-10-01).
#
# 사용: scripts/tos_avail_truth/run.sh <출력폴더> [초=1200]
#       끝나면 수집기가 한 틱 더 돈 뒤(1분):
#       python3 scripts/tos_avail_truth/analyze.py <출력폴더> <비교 시작 순간 UTC, 예 2026-10-01T00:55:42+00:00>
#
# truth.sql = 로그인 ∧ 바쁜 작업지시 없음(2초마다, 실측 858 gets·7ms) · stops.sql = 장비정지(30초마다, 1,789 gets).
# 정지는 드물게 바뀌어 30초 간격으로 뺀다. 부하는 TOS 자신의 같은 조회(초당 ~12회)의 1% 미만이었다.
# 분모 = (트럭, 2초 표본 순간) 쌍. tos_avail_check(매분 한 점)로는 1~2초 머묾을 못 보므로 이걸로 잰다.
set -u
OUT=${1:?출력 폴더}; DUR=${2:-1200}
HERE=$(cd "$(dirname "$0")" && pwd)
T="$HERE/../../tools/oracle-toolbox/scripts/remote-toolbox-sql"
mkdir -p "$OUT"; : > "$OUT/truth.jsonl"; : > "$OUT/stops.jsonl"; : > "$OUT/err.log"
end=$((SECONDS+DUR)); last_stop=-100
while [ $SECONDS -lt $end ]; do
  s=$(date +%s.%N)
  if [ $((SECONDS-last_stop)) -ge 30 ]; then "$T" oracle-prod --file "$HERE/stops.sql" >> "$OUT/stops.jsonl" 2>>"$OUT/err.log"; echo >> "$OUT/stops.jsonl"; last_stop=$SECONDS; fi
  "$T" oracle-prod --file "$HERE/truth.sql" >> "$OUT/truth.jsonl" 2>>"$OUT/err.log"; echo >> "$OUT/truth.jsonl"
  e=$(date +%s.%N); python3 -c "import time; d=2-($e-$s); time.sleep(d) if d>0 else None"
done
echo done

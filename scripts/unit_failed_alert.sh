#!/usr/bin/env bash
# systemd OnFailure 훅 — 실패한 유닛 이름을 받아 ops_alert 에 한 줄 남긴다.
#
# 왜 필요한가: 자료를 받아오는 유닛은 전부 Type=oneshot 이고 Restart= 가 없다. 한 번 실패하면
# 재시도 없이 다음 주기까지 그 구간이 비는데, 종전 감지 수단은 "사람이 화면을 본다" 뿐이었다
# (Prometheus·Sentry·SMTP·OnFailure 전부 0건). 이 훅이 그 구멍을 메운다.
#
# 심각도가 warn 인 이유: 한 번의 실패는 대개 일시적이고(Oracle 지연 편차가 크다), 다음 주기에
# 저절로 낫는다. **지속되는 결손**은 다른 장치가 crit 으로 잡는다 — 배차 쪽은 작업목록 신선도
# 게이트(300초), 표 단위는 DEADMAN. 한 번 실패까지 crit 으로 올리면 경보가 흔해져 무뎌진다.
#
# 사용: OnFailure=tt-unit-failed@%n.service  (%n = 실패한 유닛의 전체 이름)
set -euo pipefail

unit="${1:?유닛 이름이 필요하다 (예: tt-workpool.service)}"
cd "$(dirname "$0")/.."

# DATABASE_URL 은 .env 에서 읽는다 — 다른 tt-* 유닛과 같은 방식이고, 저장소에 자격증명을
# 남기지 않기 위해서다.
set -a
# shellcheck disable=SC1091
. ./.env
set +a

result=$(systemctl --user show "$unit" -p Result --value 2>/dev/null || echo unknown)
status=$(systemctl --user show "$unit" -p ExecMainStatus --value 2>/dev/null || echo '?')
# 마지막 3줄이면 원인 대부분이 잡힌다. 경보 상세가 길어지면 화면에서 잘리므로 400자로 자른다.
# ANSI 색상코드를 지운다 — tracing 이 색을 넣어 보내서, 안 지우면 상세가 \x1B[2m 범벅이 된다.
tail=$(journalctl --user -u "$unit" -n 3 --no-pager -o cat 2>/dev/null \
       | sed -E 's/\x1B\[[0-9;]*[mK]//g' | tr '\n\t' '  ' | cut -c1-400)

psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -v unit="$unit" \
  -v detail="result=${result} exit=${status} | ${tail}" <<'SQL'
INSERT INTO ops_alert (source, subject, severity, message, detail)
VALUES ('unit', :'unit', 'warn', '유닛 실행 실패 — 재시도가 없어 다음 주기까지 결손', :'detail')
ON CONFLICT (source, subject) DO UPDATE
   SET last_ts     = now(),
       occurrences = ops_alert.occurrences + 1,
       severity    = EXCLUDED.severity,
       message     = EXCLUDED.message,
       detail      = EXCLUDED.detail;
SQL

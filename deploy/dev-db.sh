#!/usr/bin/env bash
# Local dev PostgreSQL as a rootless podman container, owned by the current user.
# Zero impact on system packages or other users' databases. Bound to localhost only.
#
#   ./deploy/dev-db.sh up      # create/start the container
#   ./deploy/dev-db.sh down    # stop & remove (keeps the data volume)
#   ./deploy/dev-db.sh nuke    # stop, remove, and delete the data volume
#   ./deploy/dev-db.sh url     # print the DATABASE_URL
set -euo pipefail

NAME=wp-tt-postgres
VOL=wp-tt-pgdata
IMAGE=docker.io/library/postgres:17
PORT=5433
USER=wp
PASS=wp
DB=wp_tt
URL="postgresql://${USER}:${PASS}@127.0.0.1:${PORT}/${DB}"

# ── 폭발 반경: 컨테이너 메모리 상한 ────────────────────────────────────────────────
# 2026-07-28 OOM 장애 후 사용자 유닛 23개에 MemoryMax 를 걸었는데, 정작 **DB 쓰기를
# 실행하는 주체**인 이 컨테이너만 무제한으로 남아 있었다(memory.max=max, swap 도 max).
# 실측(컨테이너 기동 3.5시간 시점): peak 4.23GiB — 그중 3.83GiB 는 회수 가능한
# 페이지캐시이고 anon 은 0.29GiB 뿐이다. 그래서 상한을 걸어도 평시엔 OOM 킬이 아니라
# 캐시 축소로 흡수된다. 32g = 실측 peak 의 7.5배.
#
# ⚠ --memory-swap 은 의도적으로 걸지 않는다. podman 에서 --memory-swap=MEM 은 스왑 0 을
#   뜻하고, 그러면 anon 이 진짜 폭주할 때 스래싱 대신 postmaster 가 즉시 OOM 킬 된다
#   = DB 중단 + 크래시 리커버리. 호스트 스왑이 7GB 뿐이라 어차피 유한하다.
# ⚠ 값을 더 낮추지 말 것: tt-scenario-web 에서 콜드 14M 만 보고 1G 를 걸었다가 warm
#   559M(40배)을 발견한 전례가 있다. peak 은 워밍업에 비례해 자란다.
MEM=32g

case "${1:-up}" in
  up)
    if podman container exists "$NAME"; then
      podman start "$NAME" >/dev/null
      echo "started existing $NAME"
    else
      podman run -d --name "$NAME" \
        -m "$MEM" \
        -e POSTGRES_USER="$USER" -e POSTGRES_PASSWORD="$PASS" -e POSTGRES_DB="$DB" \
        -p 127.0.0.1:${PORT}:5432 \
        -v "${VOL}:/var/lib/postgresql/data" \
        "$IMAGE" >/dev/null
      echo "created $NAME (memory $MEM)"
    fi
    # 이미 존재하던 컨테이너에도 상한을 멱등 적용한다. podman update 는 실행 중 cgroup 에
    # 즉시 반영되고 재시작이 없다 — 파일만 고치면 지금 도는 컨테이너는 계속 무제한이다.
    podman update -m "$MEM" "$NAME" >/dev/null 2>&1 \
      && echo "memory cap $MEM applied" \
      || echo "WARN: podman update -m 실패 — 상한 미적용" >&2
    echo "waiting for readiness..."
    for _ in $(seq 1 30); do
      if podman exec "$NAME" pg_isready -U "$USER" -d "$DB" >/dev/null 2>&1; then
        echo "ready: $URL"; exit 0
      fi
      sleep 1
    done
    echo "timed out waiting for postgres" >&2; exit 1
    ;;
  down) podman stop "$NAME" >/dev/null 2>&1 || true; podman rm "$NAME" >/dev/null 2>&1 || true; echo "removed $NAME (volume kept)";;
  nuke) podman stop "$NAME" >/dev/null 2>&1 || true; podman rm "$NAME" >/dev/null 2>&1 || true; podman volume rm "$VOL" >/dev/null 2>&1 || true; echo "removed $NAME and volume $VOL";;
  url) echo "$URL";;
  *) echo "usage: $0 {up|down|nuke|url}" >&2; exit 2;;
esac

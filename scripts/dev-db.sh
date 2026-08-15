#!/usr/bin/env bash
# Local Postgres for amk-store development and tests.
#
# Deliberately NOT on 5432 and deliberately named: this container is a development dependency of
# this repo and nothing else, so it must be obvious what it is and safe to delete. Production runs
# CloudNativePG in k3s (see the plan's Deployment section); nothing here is a production artifact.
#
#   scripts/dev-db.sh up      start it (idempotent)
#   scripts/dev-db.sh down    stop and REMOVE it, including its data
#   scripts/dev-db.sh dsn     print the DSN for AMK_DATABASE_URL
#   scripts/dev-db.sh psql    open a shell against it
set -uo pipefail

NAME=amk-dev-postgres
PORT=55432
IMAGE=postgres:17-alpine
# A throwaway local password for a container bound to loopback. It is not a secret in any
# meaningful sense and must never be reused anywhere that matters.
PW=amk-dev-local
DSN="postgres://amk:${PW}@127.0.0.1:${PORT}/amk"

case "${1:-up}" in
  up)
    if [ -n "$(docker ps -q -f "name=^${NAME}$")" ]; then
      echo "already running: $NAME on ${PORT}"
    else
      docker rm -f "$NAME" >/dev/null 2>&1
      docker run -d --name "$NAME" \
        -p "127.0.0.1:${PORT}:5432" \
        -e POSTGRES_USER=amk -e POSTGRES_PASSWORD="$PW" -e POSTGRES_DB=amk \
        "$IMAGE" >/dev/null || exit 1
      printf 'starting'
      for _ in $(seq 1 60); do
        if docker exec "$NAME" pg_isready -U amk -d amk >/dev/null 2>&1; then
          printf ' ready\n'; break
        fi
        printf '.'; sleep 1
      done
    fi
    docker exec "$NAME" psql -U amk -d amk -tAc 'select version()' 2>/dev/null | head -1
    echo "AMK_DATABASE_URL=$DSN"
    ;;
  down)
    docker rm -f "$NAME" >/dev/null 2>&1 && echo "removed $NAME (data gone)" || echo "not running"
    ;;
  dsn) echo "$DSN" ;;
  psql) exec docker exec -it "$NAME" psql -U amk -d amk ;;
  *) echo "usage: $0 {up|down|dsn|psql}" >&2; exit 2 ;;
esac

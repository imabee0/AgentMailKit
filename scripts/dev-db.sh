#!/usr/bin/env bash
# Local Postgres for amk-store development and tests.
#
# Deliberately NOT on 5432 and deliberately named: this cluster is a development dependency of
# this repo and nothing else, so it must be obvious what it is and safe to delete. Production runs
# CloudNativePG in k3s (see the plan's Deployment section); nothing here is a production artifact.
#
#   scripts/dev-db.sh up      start it (idempotent)
#   scripts/dev-db.sh down    stop and REMOVE it, including its data
#   scripts/dev-db.sh dsn     print the DSN for AMK_DATABASE_URL
#   scripts/dev-db.sh psql    open a shell against it
#
# WHY NO DOCKER. This used to `docker run postgres:17-alpine`, which made every DB-backed
# `amk-store`/`amk-http` integration test unrunnable wherever there is no Docker daemon — including
# Claude's cloud sandbox, which has the `docker` client but no daemon behind it. `check.sh` then
# exits PASS having silently skipped that whole suite, which is the precise failure mode
# `CLAUDE.md`'s sandbox section exists to warn about: a gate that degrades quietly instead of
# failing. The server binaries Postgres ships are enough on their own, so this drives `initdb` and
# `pg_ctl` directly and the suite runs in both places. The DSN, port, role, database name and
# output lines are unchanged, so nothing that consumed this script had to change with it.
set -uo pipefail

PORT=55432
# A throwaway local password for a cluster bound to loopback. It is not a secret in any meaningful
# sense and must never be reused anywhere that matters.
PW=amk-dev-local
DSN="postgres://amk:${PW}@127.0.0.1:${PORT}/amk"

REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# `initdb` and `postgres` refuse to run as root, by design. When this script IS root (containers,
# CI images, the sandbox) it owns the cluster as a dedicated unprivileged user instead of failing;
# when it is not, it runs as the invoking user and keeps the cluster inside the repo.
if [ "$(id -u)" -eq 0 ]; then
  RUNAS=amkpg
  PGROOT=/var/lib/amk-dev-db
else
  RUNAS=""
  PGROOT="$REPO/.amk-dev-db"
fi
PGDATA="$PGROOT/data"
PGLOG="$PGROOT/server.log"

# Postgres server binaries are not on PATH on most distributions — Debian/Ubuntu hide them under
# /usr/lib/postgresql/<version>/bin, RHEL under /usr/pgsql-<version>/bin, Homebrew under its own
# prefix. Prefer whatever is already on PATH, then take the highest version found.
find_pg_bin() {
  if command -v pg_ctl >/dev/null 2>&1 && command -v initdb >/dev/null 2>&1; then
    dirname "$(command -v pg_ctl)"
    return 0
  fi
  local d
  for d in $(ls -d /usr/lib/postgresql/*/bin /usr/pgsql-*/bin \
                   /opt/homebrew/opt/postgresql*/bin /usr/local/opt/postgresql*/bin \
                   /Library/PostgreSQL/*/bin 2>/dev/null | sort -Vr); do
    if [ -x "$d/pg_ctl" ] && [ -x "$d/initdb" ]; then
      echo "$d"
      return 0
    fi
  done
  return 1
}

PGBIN="$(find_pg_bin)" || {
  echo "no PostgreSQL server binaries found (looked on PATH, /usr/lib/postgresql/*/bin," >&2
  echo "/usr/pgsql-*/bin and the Homebrew prefixes)." >&2
  echo "install a Postgres SERVER package — the client alone is not enough:" >&2
  echo "  Debian/Ubuntu  apt-get install -y postgresql" >&2
  echo "  RHEL/Fedora    dnf install -y postgresql-server" >&2
  echo "  macOS          brew install postgresql@17" >&2
  exit 1
}
PSQL="$PGBIN/psql"

# Run a command as the cluster's owner. Root must drop privileges; everyone else already is it.
as_pg() {
  if [ -n "$RUNAS" ]; then
    su "$RUNAS" -c "PATH=$PGBIN:\$PATH $*"
  else
    PATH="$PGBIN:$PATH" bash -c "$*"
  fi
}

port_is_open() { timeout 1 bash -c "(exec 3<>/dev/tcp/127.0.0.1/${PORT}) 2>/dev/null"; }

ensure_owner() {
  if [ -n "$RUNAS" ] && ! id -u "$RUNAS" >/dev/null 2>&1; then
    useradd -r -m -d "/home/$RUNAS" -s /bin/bash "$RUNAS" >/dev/null 2>&1 \
      || { echo "could not create the unprivileged cluster owner '$RUNAS'" >&2; exit 1; }
  fi
  mkdir -p "$PGROOT" || exit 1
  # The socket lives in /tmp rather than the default /var/run/postgresql, which only exists (and is
  # only writable) when a distribution's own postgres service owns the machine.
  if [ -n "$RUNAS" ]; then
    chown -R "$RUNAS" "$PGROOT" || exit 1
  fi
  chmod 700 "$PGROOT" || exit 1
}

case "${1:-up}" in
  up)
    if port_is_open; then
      echo "already running: 127.0.0.1:${PORT}"
    else
      ensure_owner
      if [ ! -s "$PGDATA/PG_VERSION" ]; then
        rm -rf "$PGDATA"
        # `--auth=trust` on a cluster that only ever listens on loopback, in a dev-only data
        # directory this script also owns deleting. The role still gets the password below so the
        # DSN is identical to the one production-shaped code expects to be handed.
        as_pg "initdb -D '$PGDATA' -U amk --auth=trust -E UTF8" >/dev/null 2>&1 || {
          echo "initdb failed" >&2; exit 1; }
      fi
      as_pg "pg_ctl -D '$PGDATA' -l '$PGLOG' \
               -o \"-p ${PORT} -k /tmp -c listen_addresses=127.0.0.1\" -w start" >/dev/null 2>&1 || {
        echo "pg_ctl start failed; last lines of $PGLOG:" >&2
        tail -20 "$PGLOG" >&2 2>/dev/null
        exit 1; }
      printf 'starting'
      for _ in $(seq 1 60); do
        if as_pg "pg_isready -h 127.0.0.1 -p ${PORT} -U amk" >/dev/null 2>&1; then
          printf ' ready\n'; break
        fi
        printf '.'; sleep 1
      done
    fi
    # Idempotent: both are no-ops on an already-provisioned cluster.
    as_pg "psql -h 127.0.0.1 -p ${PORT} -U amk -d postgres -tAc \
      \"select 1 from pg_database where datname='amk'\"" 2>/dev/null | grep -q 1 \
      || as_pg "createdb -h 127.0.0.1 -p ${PORT} -U amk -O amk amk" >/dev/null 2>&1
    as_pg "psql -h 127.0.0.1 -p ${PORT} -U amk -d postgres -qc \
      \"alter role amk with password '${PW}'\"" >/dev/null 2>&1
    as_pg "psql -h 127.0.0.1 -p ${PORT} -U amk -d amk -tAc 'select version()'" 2>/dev/null | head -1
    echo "AMK_DATABASE_URL=$DSN"
    ;;
  down)
    if [ -s "$PGDATA/PG_VERSION" ]; then
      as_pg "pg_ctl -D '$PGDATA' -m immediate -w stop" >/dev/null 2>&1
    fi
    rm -rf "$PGROOT" && echo "removed the dev cluster at $PGROOT (data gone)"
    ;;
  dsn) echo "$DSN" ;;
  psql) exec "$PSQL" "$DSN" ;;
  *) echo "usage: $0 {up|down|dsn|psql}" >&2; exit 2 ;;
esac

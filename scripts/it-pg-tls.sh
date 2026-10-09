#!/usr/bin/env bash
# Start a TLS-only PostgreSQL for the integration tests (SU_IT_PG_TLS).
#
# Usage: scripts/it-pg-tls.sh [dir]   (certificates go to dir, default ./target/it-tls)
#
# Generates a throw-away CA and a server certificate for `localhost`, then runs
# container `su-it-pg-tls` on port 45433: user `postgres`, password
# `a@b:c/d#e?f%g`, plain-text connections rejected by pg_hba.conf.
#
# The files are not bind-mounted: on Windows (Docker Desktop) a mounted key
# file can't be given the owner and 0600 mode PostgreSQL insists on. They are
# `docker cp`-ed into the created container instead, and its entrypoint copies
# them to /tls owned by `postgres` before starting the server. Works the same
# on Linux and macOS.
set -euo pipefail
# Git Bash would rewrite `/CN=...` and `container:/path` as Windows paths.
export MSYS_NO_PATHCONV=1

dir="${1:-target/it-tls}"
name=su-it-pg-tls
mkdir -p "$dir"
cd "$dir"

openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=su-test-ca" \
  -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
  -keyout server.key -out server.csr 2>/dev/null
printf "subjectAltName=DNS:localhost\n" > san.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 \
  -extfile san.ext -out server.crt 2>/dev/null
# `local` keeps the image's init scripts (Unix socket) working.
printf "hostssl all all all scram-sha-256\nhostnossl all all all reject\nlocal all all trust\n" \
  > pg_hba.conf

docker rm -f "$name" >/dev/null 2>&1 || true
docker create --name "$name" -p 45433:5432 \
  -e POSTGRES_PASSWORD='a@b:c/d#e?f%g' \
  --entrypoint sh postgres:16-alpine -c '
    install -d -o postgres -g postgres /tls &&
    install -o postgres -g postgres -m 600 \
      /tls-src/server.crt /tls-src/server.key /tls-src/pg_hba.conf /tls &&
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key \
      -c hba_file=/tls/pg_hba.conf' >/dev/null
docker cp . "$name:/tls-src"
docker start "$name" >/dev/null

# Windows path when available, so the tests (a native program) can open it.
ca="$(pwd -W 2>/dev/null || pwd)/ca.pem"
echo "SU_IT_PG_TLS=localhost:45433 SU_IT_PG_TLS_CA=$ca"

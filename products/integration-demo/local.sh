#!/bin/sh
# The integration product's
# `local` profile on one machine -- Flight Control spawning its workers as local
# processes, the hub on a Unix socket, one shared secret for the platform plane.
#
# Starts a mock IdP (nginx over TLS from a throwaway CA) and Postgres in Docker,
# generates and builds `local`, runs Flight Control with a private HOME so its
# sockets and state live in a temporary directory, and checks: workers register
# with the Directory through the `unix://` hub endpoint and keep heartbeating;
# the consumer resolves its provider; the edge publishes only the exposed
# operation on loopback; the platform plane wants the shared secret; a killed
# worker is respawned and registers again; the socket directory is 0700 and the
# socket 0600. Everything it starts is removed at the end unless KEEP=1.
#
#   GEARS_RUST=../gears-rust products/integration-demo/local.sh
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
cd "$repo"
gears_rust=$(CDPATH= cd -- "${GEARS_RUST:-../gears-rust}" && pwd)
gearbox=${GEARBOX:-$repo/target/debug/gearbox}
pg_image=${PG_IMAGE:-postgres:16.4-alpine}
idp_image=${IDP_IMAGE:-nginx:1.27.5-alpine}
# The ports the description names: the issuer and the database.
idp_port=58443
pg_port=55442
issuer=https://localhost:$idp_port
audience=integration-demo
out=.gearbox/integration-demo/local
pass=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; [ -n "${log:-}" ] && tail -40 "$log"; exit 1; }

for port in $idp_port $pg_port 8087 8088; do
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    fail "port $port is taken; this check binds it"
  fi
done

# Short on purpose: the hub sockets live under $work/home, and a Unix socket
# path is limited to 104 bytes on macOS (108 on Linux).
work=$(mktemp -d /tmp/gbx-a23.XXXXXX)
suffix=$$
idp=gbx-local-idp-$suffix
pg=gbx-local-pg-$suffix
host_pid=
log=
cleanup() {
  if [ -n "$host_pid" ]; then
    # The workers too: one whose host was killed outlives it, and would hold
    # its port against the next run.
    workers=$(pgrep -P "$host_pid" || true)
    kill "$host_pid" 2>/dev/null || true
    wait "$host_pid" 2>/dev/null || true
    [ -z "$workers" ] || kill $workers 2>/dev/null || true
  fi
  if [ "${KEEP:-0}" != 1 ]; then
    docker rm -f "$idp" "$pg" >/dev/null 2>&1 || true
    rm -rf "$work"
  else
    echo "kept: $work, containers $idp $pg"
  fi
}
trap cleanup EXIT INT TERM

echo "== mock IdP ($issuer) and Postgres in Docker"
(
  cd "$work"
  openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 1 -subj /CN=a23-test-ca \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -keyout tls.key -out tls.csr -subj /CN=localhost 2>/dev/null
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' >tls.ext
  openssl x509 -req -in tls.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out tls.crt -days 1 -extfile tls.ext 2>/dev/null
  openssl genrsa -out sign.key 2048 2>/dev/null
  openssl genrsa -out foreign.key 2048 2>/dev/null
  chmod 0644 tls.key
)
cat >"$work/jwt.py" <<'EOF'
import base64, json, os, subprocess, sys, time, uuid
def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()
work, cmd = sys.argv[1], sys.argv[2]
if cmd == "documents":
    issuer = sys.argv[3]
    modulus = subprocess.check_output(
        ["openssl", "rsa", "-in", work + "/sign.key", "-noout", "-modulus"]).decode().strip()
    n = b64(bytes.fromhex(modulus.split("=", 1)[1]))
    jwk = {"kty": "RSA", "kid": "a23", "use": "sig", "alg": "RS256", "n": n, "e": "AQAB"}
    os.makedirs(work + "/idp/.well-known", exist_ok=True)
    json.dump({"keys": [jwk]}, open(work + "/idp/jwks.json", "w"))
    json.dump({"issuer": issuer, "jwks_uri": issuer + "/jwks.json",
               "token_endpoint": issuer + "/token",
               "id_token_signing_alg_values_supported": ["RS256"]},
              open(work + "/idp/.well-known/openid-configuration", "w"))
else:  # token <key> <issuer> <audience> <seconds to expiry>
    key, iss, aud, ttl = sys.argv[3], sys.argv[4], sys.argv[5], int(sys.argv[6])
    now = int(time.time())
    header = b64(json.dumps({"alg": "RS256", "typ": "JWT", "kid": "a23"}).encode())
    claims = {"iss": iss, "aud": aud, "sub": str(uuid.uuid4()), "tenant_id": str(uuid.uuid4()),
              "scope": "openid", "iat": now - 60, "nbf": now - 60, "exp": now + ttl}
    payload = b64(json.dumps(claims).encode())
    signing_input = (header + "." + payload).encode()
    signature = subprocess.run(["openssl", "dgst", "-sha256", "-sign", work + "/" + key],
                               input=signing_input, capture_output=True, check=True).stdout
    print(header + "." + payload + "." + b64(signature))
EOF
python3 "$work/jwt.py" "$work" documents "$issuer"
token() { python3 "$work/jwt.py" "$work" token "$@"; }
cat >"$work/nginx.conf" <<'EOF'
server {
  listen 8443 ssl;
  ssl_certificate /tls/tls.crt;
  ssl_certificate_key /tls/tls.key;
  root /idp;
  default_type application/json;
  location / { try_files $uri =404; }
}
EOF
docker run -d --name "$idp" -p "127.0.0.1:$idp_port:8443" \
  -v "$work/nginx.conf:/etc/nginx/conf.d/default.conf:ro" \
  -v "$work/tls.crt:/tls/tls.crt:ro" -v "$work/tls.key:/tls/tls.key:ro" \
  -v "$work/idp:/idp:ro" "$idp_image" >/dev/null
password=$(openssl rand -hex 16)
docker run -d --name "$pg" -p "127.0.0.1:$pg_port:5432" \
  -e POSTGRES_USER=integration -e POSTGRES_DB=types_registry -e POSTGRES_PASSWORD="$password" \
  "$pg_image" >/dev/null
start=$(date +%s)
until docker exec "$pg" pg_isready -U integration -d types_registry >/dev/null 2>&1 \
  && curl -sf --cacert "$work/ca.crt" "$issuer/.well-known/openid-configuration" >/dev/null; do
  [ $(( $(date +%s) - start )) -lt 60 ] || fail "the IdP or Postgres did not come up in 60s"
  sleep 1
done
ok "mock IdP serves discovery over TLS from a private CA; Postgres is ready"

echo "== generate and build \`local\`"
"$gearbox" generate --root "$gears_rust" --product products/integration-demo/product.gdl \
  --profile local >"$work/generate.log" 2>&1 || { cat "$work/generate.log"; fail "gearbox generate --profile local"; }
# From inside the tree: Cargo reads `.cargo/config.toml` (the target directory
# the host spawns workers from) relative to where it runs, not to the manifest.
(cd "$out" && cargo build --workspace) >"$work/build.log" 2>&1 \
  || { tail -40 "$work/build.log"; fail "cargo build of the generated workspace"; }
ok "generated and built: $(ls "$out/apps" | tr '\n' ' ')"
# The description names the CA by a path relative to the processes' working
# directory, as the chart mounts it at /app/oidc-ca on Kubernetes.
mkdir -p "$out/oidc-ca"
cp "$work/ca.crt" "$out/oidc-ca/ca.crt"

port_of() { sed -n "s/^  listen_addr: 127\.0\.0\.1:\([0-9]*\)$/\1/p" "$out/config/$1.yaml"; }
payments_port=$(port_of payments)
checkout_port=$(port_of checkout)
[ -n "$payments_port" ] && [ -n "$checkout_port" ] || fail "the generated worker configs no longer name a loopback listen_addr"
for port in $payments_port $checkout_port; do
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    fail "port $port (a worker's listener) is taken; a worker from an earlier run?"
  fi
done
binary=$(sed -n 's/^target-dir = "\(.*\)"$/\1/p' "$out/.cargo/config.toml")
binary=$(CDPATH= cd -- "$out/$binary" && pwd)/debug/gbx-flight-control
[ -x "$binary" ] || fail "no Flight Control binary at $binary"

echo "== Flight Control with its workers, one shared secret"
secret=$(openssl rand -hex 32)
home=$work/home
mkdir -p "$home"
log=$work/flight-control.log
start_host() {
  (
    cd "$out"
    # Every `internal_auth` block names `${TOOLKIT_PLATFORM_SECRET}`; the workers
    # inherit it from Flight Control.
    exec env HOME="$home" GEARS_PG_PASSWORD="$password" TOOLKIT_PLATFORM_SECRET="$secret" \
      "$binary" --config config/flight-control.yaml
  ) >>"$log" 2>&1 &
  host_pid=$!
}
start_host

edge=http://127.0.0.1:8087
internal=http://127.0.0.1:8088
payments=http://127.0.0.1:$payments_port
checkout=http://127.0.0.1:$checkout_port
status() {
  method=$1; url=$2; shift 2
  curl -s -o /dev/null -w "%{http_code}" -m 10 -X "$method" "$@" "$url" 2>/dev/null || true
}
expect() {
  want=$1; what=$2; shift 2
  got=$(status "$@")
  [ "$got" = "$want" ] || fail "$what: $got, expected $want"
  ok "$want $what"
}
eventually() {
  limit=$1; want=$2; what=$3; shift 3
  start=$(date +%s)
  while :; do
    kill -0 "$host_pid" 2>/dev/null || fail "Flight Control exited"
    got=$(status "$@")
    [ "$got" = "$want" ] && break
    [ $(( $(date +%s) - start )) -lt "$limit" ] || fail "$what: still $got after ${limit}s, expected $want"
    sleep 1
  done
  ok "$want $what (after $(( $(date +%s) - start ))s)"
}
platform="x-toolkit-internal-token: $secret"
valid=$(token sign.key "$issuer" "$audience" 3600)
bearer="Authorization: Bearer $valid"
json="Content-Type: application/json"
charge='{"amount_cents":100,"currency":"USD","description":"a23"}'
evaluate='{"subject":{"id":"f0000000-0000-4000-8000-000000000001","type":"user","properties":{}},"action":{"name":"get"},"resource":{"type":"gts.cf.core.users.user.v1~","properties":{}},"context":{}}'
# The Directory's records, as `name endpoint state` lines.
records() {
  curl -s -m 10 -H "$bearer" "$internal/service-discovery/v1/gears" | python3 -c '
import json, sys
data = json.load(sys.stdin)
items = data.get("items", data) if isinstance(data, dict) else data
for g in items:
    for i in g.get("instances", []):
        print(g.get("name"), i.get("rest_endpoint") or "", i.get("state") or "")'
}

eventually 120 200 "edge: checkout's exposed charge, routed to checkout and on to payments" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"

echo "== registration through the unix:// hub"
grep -q 'unix:' "$log" || fail "no log line names a unix: endpoint"
grep -Eq '(directory|TOOLKIT_DIRECTORY_ENDPOINT)[^ ]*[ =:"]+unix:' "$log" \
  || fail "the workers were not given a unix: Directory endpoint: $(grep -m3 -i directory "$log")"
ok "the workers reach the Directory through the hub's unix: socket"
# Cluster is linked into checkout on this profile, so it serves nothing out.
for name in api-contracts authz-resolver api-contracts-consumer; do
  records | grep -q "^$name http://127.0.0.1:" || fail "the Directory has no loopback record for $name: $(records)"
done
ok "the Directory keeps a loopback record for every gear served out: $(records | cut -d' ' -f1 | sort -u | tr '\n' ' ')"
# A record reads `healthy` only after a heartbeat (`registered` before one).
start=$(date +%s)
until [ "$(records | awk '$1 != "service-discovery" && $1 != "types-registry" && $3 != "healthy"' | wc -l | tr -d ' ')" = 0 ]; do
  [ $(( $(date +%s) - start )) -lt 60 ] || fail "not every worker record turned healthy by heartbeat: $(records)"
  sleep 1
done
ok "every worker record is healthy: heartbeats arrive through the socket"

echo "== the edge, on loopback"
expect 401 "edge: charge without a token" POST "$edge/api-contracts-consumer/v1/charge" -H "$json" -d "$charge"
expect 401 "edge: charge signed by a key outside the JWKS" POST "$edge/api-contracts-consumer/v1/charge" \
  -H "Authorization: Bearer $(token foreign.key "$issuer" "$audience" 3600)" -H "$json" -d "$charge"
expect 404 "edge: internal GET status is not routed" GET "$edge/api-contracts-consumer/v1/status" -H "$bearer"
expect 404 "edge: Directory GET gears is not routed" GET "$edge/service-discovery/v1/gears" -H "$bearer"
expect 404 "edge: AuthZ evaluate is not routed, even with the secret" \
  POST "$edge/authz-resolver/v1/evaluate" -H "$platform" -H "$json" -d "$evaluate"
for pid in $host_pid $(pgrep -P "$host_pid" || true); do
  wide=$(lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 {print $9}' | grep -v '^127\.0\.0\.1:' || true)
  [ -z "$wide" ] || fail "process $pid listens beyond loopback: $wide"
done
ok "Flight Control and every worker listen on loopback only"

echo "== direct calls and the platform plane"
expect 200 "direct: checkout's status round-trips through cluster over gRPC" \
  GET "$checkout/api-contracts-consumer/v1/status" -H "$bearer"
expect 401 "direct: payments refuses a call without a tenant token" \
  POST "$payments/api-contracts/v1/payments/charge" -H "$json" -d "$charge"
expect 401 "AuthZ evaluate: no credential" POST "$payments/authz-resolver/v1/evaluate" -H "$json" -d "$evaluate"
expect 401 "AuthZ evaluate: a wrong secret" POST "$payments/authz-resolver/v1/evaluate" \
  -H "x-toolkit-internal-token: forged" -H "$json" -d "$evaluate"
expect 401 "AuthZ evaluate: a tenant bearer is the other plane" POST "$payments/authz-resolver/v1/evaluate" \
  -H "$bearer" -H "$json" -d "$evaluate"
expect 200 "AuthZ evaluate: the shared secret" POST "$payments/authz-resolver/v1/evaluate" \
  -H "$platform" -H "$json" -d "$evaluate"

echo "== sockets"
run=$home/.cf-gears/integration-demo/local/run
for app in flight-control checkout; do
  dir=$run/$app
  [ -S "$dir/grpc-hub.sock" ] || fail "no hub socket at $dir/grpc-hub.sock"
  mode=$(stat -f '%Lp' "$dir")
  [ "$mode" = 700 ] || fail "$dir is $mode, expected 700"
  mode=$(stat -f '%Lp' "$dir/grpc-hub.sock")
  [ "$mode" = 600 ] || fail "$dir/grpc-hub.sock is $mode, expected 600"
done
ok "each hub's socket directory is 0700 and its socket 0600"

echo "== a worker that dies fails visibly; a restart brings every worker back"
victim=$(pgrep -P "$host_pid" -f gbx-payments | head -1)
[ -n "$victim" ] || fail "no payments worker under Flight Control"
kill -9 "$victim"
start=$(date +%s)
while :; do
  got=$(status POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge")
  case $got in 5??|404) break ;; esac
  [ $(( $(date +%s) - start )) -lt 60 ] || fail "with payments dead the charge still answers $got"
  sleep 1
done
ok "with payments killed the charge fails with $got rather than hanging (no worker is respawned)"
workers=$(pgrep -P "$host_pid" || true)
kill "$host_pid"
wait "$host_pid" 2>/dev/null || true
start=$(date +%s)
for pid in $workers; do
  while kill -0 "$pid" 2>/dev/null; do
    [ $(( $(date +%s) - start )) -lt 20 ] || fail "worker $pid outlived Flight Control by 20s"
    sleep 1
  done
done
ok "stopping Flight Control stops its workers"
host_pid=
start_host
eventually 120 200 "edge: the charge after Flight Control restarted with its workers" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
start=$(date +%s)
until records | grep -q "^authz-resolver .* healthy$"; do
  [ $(( $(date +%s) - start )) -lt 60 ] || fail "payments did not register again: $(records)"
  sleep 1
done
ok "payments registered again and heartbeats: $(records | grep '^authz-resolver')"

echo "== $pass checks passed"

#!/bin/sh
# Phase 4 acceptance of the integration product (tasks/service-deployment/phase-4.md).
#
# Generates `kind`, installs it into its own namespace `gearbox-p4` (the issuer
# in product.gdl names it) beside a mock IdP and a Postgres, and checks direct
# routing and both authentication planes on a real cluster: what the edge
# publishes, tenant and platform calls with their negatives, calls that keep
# working when the edge stops routing, Flight Control and provider
# replacement, the AuthZ entrypoint from inside the cluster, and the embedded
# all-in-one binary. The namespace is refused if it exists, and removed at the
# end unless KEEP=1.
#
# Outside the namespace it creates exactly the TokenReview RBAC the charts
# declare -- a ClusterRole and ClusterRoleBinding per unit, named
# `gearbox-p4-<serviceaccount>-tokenreview` -- and removes them with the
# release. So it runs only on a cluster declared disposable with
# DISPOSABLE_CLUSTER=1, and refuses if any of those objects already exists
# (gears-rust deploy/helm/cluster-scoped-guard.sh).
#
#   DISPOSABLE_CLUSTER=1 HELM=... KUBE_CONTEXT=docker-desktop LOAD_NODE=desktop-control-plane \
#   GEARS_RUST=../gears-rust products/integration-demo/acceptance.sh
#
# Images (build them first): flight-control:p4 and cluster-oop:p4 from
# gears-rust (FC_IMAGE, CLUSTER_IMAGE), gbx-{payments,checkout}:0.1.0 from
# the generated `docker/build.sh`, plus nginx, postgres and curl.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
cd "$repo"
helm=${HELM:-helm}
ctx=${KUBE_CONTEXT:-docker-desktop}
ns=gearbox-p4
gears_rust=$(CDPATH= cd -- "${GEARS_RUST:-../gears-rust}" && pwd)
fc_image=${FC_IMAGE:-flight-control:p4}
cl_image=${CLUSTER_IMAGE:-cluster-oop:p4}
pg_image=${PG_IMAGE:-postgres:16.4-alpine}
curl_image=${CURL_IMAGE:-curlimages/curl:8.10.1}
idp_image=${IDP_IMAGE:-nginx:1.27.5-alpine}
release=p4
issuer=https://mock-idp.$ns.svc.cluster.local
audience=integration-demo
chart=.gearbox/integration-demo/kind/helm/integration-demo
k() { kubectl --context "$ctx" -n "$ns" "$@"; }
h() { "$helm" --kube-context "$ctx" -n "$ns" "$@"; }
pass=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; exit 1; }

phase=$(kubectl --context "$ctx" get namespace "$ns" -o jsonpath='{.status.phase}' 2>/dev/null || true)
if [ "$phase" = Terminating ]; then
  kubectl --context "$ctx" wait --for=delete "namespace/$ns" --timeout=180s >/dev/null
  phase=
fi
[ -z "$phase" ] || fail "namespace $ns already exists (this check removes what it creates)"
created=
work=$(mktemp -d)
forward=
cleanup() {
  [ -n "$forward" ] && kill "$forward" 2>/dev/null || true
  if [ -n "$created" ] && [ "${KEEP:-0}" != 1 ]; then
    h uninstall "$release" >/dev/null 2>&1 || true
    kubectl --context "$ctx" delete namespace "$ns" --wait=false >/dev/null 2>&1 || true
  fi
  [ "${KEEP:-0}" = 1 ] && echo "kept: namespace $ns, work dir $work" || rm -rf "$work"
}
trap cleanup EXIT

echo "== generate"
cargo run -q -p gearbox-cli -- generate --root "$gears_rust" --source-id gears-rust \
  --product products/integration-demo/product.gdl --profile kind >/dev/null
# `update`, not `build`: every dependency is a generated `file://` subchart, and
# a lock left by an earlier generation with other units would refuse `build`.
"$helm" dependency update "$chart" >/dev/null
# A worker's port, as the generator assigned it (its Service listens on it too).
port_of() {
  sed -n 's#^  advertise_uri: http://[^:]*:\([0-9]*\)$#\1#p' ".gearbox/integration-demo/kind/config/$1.yaml"
}
payments_port=$(port_of payments)
checkout_url=http://checkout:$(port_of checkout)
payments_url=http://payments:$payments_port
[ -n "$payments_port" ] && [ "$checkout_url" != http://checkout: ] || fail "no worker ports in the generated config"
for name in payments checkout; do
  grep -q 'audience: toolkit-internal' "$chart/values.yaml" \
    || fail "the generated values no longer mount the platform token as this script assumes"
done

if [ -n "${LOAD_NODE:-}" ]; then
  for image in "$fc_image" "$cl_image" gbx-payments:0.1.0 gbx-checkout:0.1.0 \
    "$pg_image" "$curl_image" "$idp_image"; do
    docker image save --platform "linux/${LOAD_ARCH:-arm64}" --output "$work/image.tar" "$image"
    docker exec -i "$LOAD_NODE" ctr --namespace k8s.io images import \
      --platform "linux/${LOAD_ARCH:-arm64}" --digests - <"$work/image.tar" >/dev/null
    rm -f "$work/image.tar"
  done
  echo "ok   images loaded into $LOAD_NODE"
fi

echo "== namespace, mock IdP, database"
kubectl --context "$ctx" create namespace "$ns" >/dev/null
created=1
KUBE_CONTEXT=$ctx NAMESPACE=$ns IDP_IMAGE=$idp_image sh "$gears_rust/deploy/helm/mock-idp.sh" "$work"
token() { python3 "$work/jwt.py" "$work" token "$@"; }
password=$(openssl rand -hex 16)
k create secret generic integration-db --from-literal=GEARS_PG_PASSWORD="$password" >/dev/null
k apply -f - >/dev/null <<EOF
apiVersion: apps/v1
kind: Deployment
metadata: {name: postgres}
spec:
  replicas: 1
  selector: {matchLabels: {app: postgres}}
  template:
    metadata: {labels: {app: postgres}}
    spec:
      containers:
        - name: postgres
          image: $pg_image
          imagePullPolicy: IfNotPresent
          env:
            - {name: POSTGRES_USER, value: integration}
            - {name: POSTGRES_DB, value: types_registry}
            - name: POSTGRES_PASSWORD
              valueFrom: {secretKeyRef: {name: integration-db, key: GEARS_PG_PASSWORD}}
          ports: [{containerPort: 5432}]
          readinessProbe:
            exec: {command: [pg_isready, -U, integration, -d, types_registry]}
            periodSeconds: 2
---
apiVersion: v1
kind: Service
metadata: {name: postgres}
spec:
  selector: {app: postgres}
  ports: [{port: 5432}]
EOF
k rollout status deploy/postgres --timeout=180s >/dev/null
ok "Postgres ready"

# The workers validate tenant tokens too, so each mounts the IdP's CA beside the
# platform token the generated chart already mounts.
worker_values() {
  cat <<EOF
  image: {pullPolicy: IfNotPresent}
  extraVolumes:
    - name: platform-token
      projected:
        sources:
          - serviceAccountToken: {audience: toolkit-internal, expirationSeconds: 3600, path: toolkit-internal}
    - {name: oidc-ca, configMap: {name: idp-ca}}
  extraVolumeMounts:
    - {name: platform-token, mountPath: /var/run/secrets/tokens, readOnly: true}
    - {name: oidc-ca, mountPath: /app/oidc-ca, readOnly: true}
EOF
}
{
  echo "flight-control:"
  echo "  image: {repository: ${fc_image%:*}, tag: ${fc_image#*:}}"
  echo "  oidc: {caConfigMap: idp-ca}"
  echo "cluster:"
  echo "  image: {repository: ${cl_image%:*}, tag: ${cl_image#*:}}"
  for name in payments checkout; do
    echo "$name:"
    worker_values
  done
} >"$work/values.yaml"

echo "== install ($ns)"
# The only objects outside the namespace: the charts' cluster-scoped RBAC. Each
# must be new.
h template "$release" "$chart" -f "$work/values.yaml" >"$work/rendered.yaml"
KUBE_CONTEXT=$ctx sh "$gears_rust/deploy/helm/cluster-scoped-guard.sh" "$work/rendered.yaml" "$ns" \
  >"$work/cluster-scoped"
ok "disposable cluster: cluster-scoped objects are new and named after $ns: $(wc -l <"$work/cluster-scoped" | tr -d ' ')"
h install "$release" "$chart" -f "$work/values.yaml" >/dev/null
for deploy in flight-control cluster "$release-payments" "$release-checkout"; do
  k rollout status "deploy/$deploy" --timeout=240s >/dev/null || {
    k get pods; k logs "deploy/$deploy" --tail=30; fail "$deploy did not become Ready"
  }
done
ok "cold start: every unit Ready (Flight Control, cluster, payments, checkout)"

# A probe inside the cluster, with its own ServiceAccount and a projected
# platform token: the caller every in-cluster check speaks as.
k create serviceaccount p4-probe >/dev/null
k apply -f - >/dev/null <<EOF
apiVersion: v1
kind: Pod
metadata: {name: probe}
spec:
  serviceAccountName: p4-probe
  automountServiceAccountToken: false
  containers:
    - name: curl
      image: $curl_image
      imagePullPolicy: IfNotPresent
      command: [sleep, "86400"]
      volumeMounts: [{name: platform-token, mountPath: /var/run/secrets/tokens, readOnly: true}]
  volumes:
    - name: platform-token
      projected:
        sources: [{serviceAccountToken: {audience: toolkit-internal, expirationSeconds: 3600, path: toolkit-internal}}]
EOF
k wait --for=condition=Ready pod/probe --timeout=120s >/dev/null

# status <method> <url> [curl args...] -- the HTTP status the probe sees (000:
# no connection). The header value PLATFORM is replaced, inside the probe, by
# its projected platform token.
status() {
  method=$1; url=$2; shift 2
  printf '%s\n' "$@" >"$work/args"
  k exec -i probe -- sh -c '
    m=$0; u=$1
    set --
    token=$(cat /var/run/secrets/tokens/toolkit-internal)
    while IFS= read -r arg; do
      [ -n "$arg" ] || continue
      case $arg in *PLATFORM*) arg=$(printf %s "$arg" | sed "s|PLATFORM|$token|") ;; esac
      set -- "$@" "$arg"
    done
    curl -s -o /dev/null -w "%{http_code}" -m 10 -X "$m" "$@" "$u"' "$method" "$url" <"$work/args" 2>/dev/null || true
}
body() { url=$1; shift; k exec probe -- curl -s -m 10 "$@" "$url"; }
expect() {
  want=$1; what=$2; shift 2
  got=$(status "$@")
  [ "$got" = "$want" ] || fail "$what: $got, expected $want"
  ok "$want $what"
}
# eventually <seconds> <want> <what> <status args...>: poll until it answers.
eventually() {
  limit=$1; want=$2; what=$3; shift 3
  start=$(date +%s)
  while :; do
    got=$(status "$@")
    [ "$got" = "$want" ] && break
    [ $(( $(date +%s) - start )) -lt "$limit" ] || fail "$what: still $got after ${limit}s, expected $want"
    sleep 1
  done
  ok "$want $what (after $(( $(date +%s) - start ))s)"
}
# The gears the directory lists, by name.
listed() {
  body "$fc_internal/service-discovery/v1/gears" -H "$bearer" 2>/dev/null | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except ValueError:
    data = []
items = data.get("items", data) if isinstance(data, dict) else data
print(" ".join(sorted({str(i.get("gear") or i.get("name") or i.get("gear_name")) for i in items if isinstance(i, dict)})))'
}

valid=$(token sign.key "$issuer" "$audience" 3600)
bearer="Authorization: Bearer $valid"
json="Content-Type: application/json"
charge='{"amount_cents":100,"currency":"USD","description":"phase 4"}'
edge=http://flight-control:8087
fc_internal=http://flight-control-internal:8088
evaluate='{"subject":{"id":"f0000000-0000-4000-8000-000000000001","type":"user","properties":{}},"action":{"name":"get"},"resource":{"type":"gts.cf.core.users.user.v1~","properties":{}},"context":{}}'
workers="api-contracts api-contracts-consumer authz-resolver cluster"

echo "== the edge publishes only exposed operations, authenticated"
eventually 60 200 "edge: checkout's exposed charge, routed to checkout and on to payments" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
expect 401 "edge: charge without a token" POST "$edge/api-contracts-consumer/v1/charge" -H "$json" -d "$charge"
expect 401 "edge: charge with an expired token" POST "$edge/api-contracts-consumer/v1/charge" \
  -H "Authorization: Bearer $(token sign.key "$issuer" "$audience" -3600)" -H "$json" -d "$charge"
expect 401 "edge: charge with another audience" POST "$edge/api-contracts-consumer/v1/charge" \
  -H "Authorization: Bearer $(token sign.key "$issuer" another 3600)" -H "$json" -d "$charge"
expect 401 "edge: charge signed by a key outside the JWKS" POST "$edge/api-contracts-consumer/v1/charge" \
  -H "Authorization: Bearer $(token foreign.key "$issuer" "$audience" 3600)" -H "$json" -d "$charge"
expect 200 "edge: a forged platform header beside a valid token is stripped, not trusted" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "x-toolkit-internal-token: forged" -H "$json" -d "$charge"
expect 401 "edge: a forged platform header alone authenticates nothing" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "x-toolkit-internal-token: forged" -H "$json" -d "$charge"
# Each internal operation by its real method, body and credentials: a 404 here
# is the route's absence, not a wrong method or a missing token.
internal_absent() {
  base=$1; where=$2
  expect 404 "$where: internal GET status is not routed" GET "$base/api-contracts-consumer/v1/status" -H "$bearer"
  expect 404 "$where: payments' POST charge is not routed" POST "$base/api-contracts/v1/payments/charge" \
    -H "$bearer" -H "$json" -d "$charge"
  expect 404 "$where: Types GET entities is not routed" GET "$base/types-registry/v1/entities" -H "$bearer"
  expect 404 "$where: Directory GET gears is not routed" GET "$base/service-discovery/v1/gears" -H "$bearer"
  expect 404 "$where: AuthZ POST evaluate is not routed, even with a platform token" \
    POST "$base/authz-resolver/v1/evaluate" -H "x-toolkit-internal-token: PLATFORM" -H "$bearer" \
    -H "$json" -d "$evaluate"
}
internal_absent "$edge" edge
paths=$(body "$edge/openapi.json" | python3 -c 'import json,sys; print(" ".join(sorted(json.load(sys.stdin).get("paths", {}))))')
[ "$paths" = "/api-contracts-consumer/v1/charge" ] || fail "edge OpenAPI lists: $paths"
ok "edge OpenAPI describes exactly the proxied exposed operation: $paths"

echo "== direct calls inside the cluster, both planes"
expect 200 "direct: checkout's internal status round-trips through cluster over gRPC" \
  GET $checkout_url/api-contracts-consumer/v1/status -H "$bearer"
expect 200 "direct: checkout charges payments with no edge in the path" \
  POST $checkout_url/api-contracts-consumer/v1/charge -H "$bearer" -H "$json" -d "$charge"
expect 401 "direct: payments refuses a call without a tenant token" \
  POST $payments_url/api-contracts/v1/payments/charge -H "$json" -d "$charge"
expect 200 "direct: payments serves a tenant call" \
  POST $payments_url/api-contracts/v1/payments/charge -H "$bearer" -H "$json" -d "$charge"
expect 401 "AuthZ evaluate: no credential" POST $payments_url/authz-resolver/v1/evaluate -H "$json" -d "$evaluate"
expect 401 "AuthZ evaluate: a forged platform token" POST $payments_url/authz-resolver/v1/evaluate \
  -H "x-toolkit-internal-token: forged" -H "$json" -d "$evaluate"
expect 401 "AuthZ evaluate: a tenant bearer is the other plane" POST $payments_url/authz-resolver/v1/evaluate \
  -H "$bearer" -H "$json" -d "$evaluate"
expect 200 "AuthZ evaluate: a ServiceAccount token for the platform audience" \
  POST $payments_url/authz-resolver/v1/evaluate -H "x-toolkit-internal-token: PLATFORM" -H "$json" -d "$evaluate"
gears=$(listed)
for gear in $workers; do
  case " $gears " in *" $gear "*) ;; *) fail "the directory lists [$gears], not $gear" ;; esac
done
ok "the directory lists every worker: $gears"
# todo-issues 1: payments keeps a record for its anchor (authz-resolver) and one
# for the PaymentApi provider linked beside it, both at its Service, so checkout
# finds PaymentApi under its own name.
body "$fc_internal/service-discovery/v1/gears" -H "$bearer" | python3 -c '
import json, sys
url = sys.argv[1]
data = json.load(sys.stdin)
items = data.get("items", data) if isinstance(data, dict) else data
for name in ("api-contracts", "authz-resolver"):
    endpoints = [i.get("rest_endpoint") or "" for g in items if g.get("name") == name
                 for i in g.get("instances", [])]
    if not any(e.startswith(url) for e in endpoints):
        sys.exit(f"{name}: {endpoints}, not at {url}")
' "http://payments.$ns.svc.cluster.local:$payments_port" \
  || fail "the directory does not keep both of payments' records at its Service"
ok "the directory keeps a record per gear payments serves out: api-contracts and authz-resolver"

echo "== the edge stops routing; direct calls continue"
k delete service flight-control >/dev/null
expect 000 "edge: unreachable (its Service is gone)" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
expect 200 "outage: checkout still charges payments directly" \
  POST $checkout_url/api-contracts-consumer/v1/charge -H "$bearer" -H "$json" -d "$charge"
# The destination: the charge's payment is in payments' own records, and the
# directory resolves the provider to the payments Service -- not to the edge.
payment=$(body $checkout_url/api-contracts-consumer/v1/charge -X POST -H "$bearer" -H "$json" -d "$charge" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_id"])')
body $payments_url/api-contracts/v1/payments -H "$bearer" | grep -q "$payment" \
  || fail "outage: payment $payment is not in payments' records"
ok "outage: the charge landed at payments (payment $payment in its records)"
body "$fc_internal/service-discovery/v1/gears" -H "$bearer" | grep -q "payments.$ns.svc.cluster.local:$payments_port" \
  || fail "outage: the directory does not resolve api-contracts to the payments Service"
ok "outage: the directory resolves PaymentApi's provider to the payments Service, no edge hop"
first=$(body $checkout_url/api-contracts-consumer/v1/status -H "$bearer" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')
second=$(body $checkout_url/api-contracts-consumer/v1/status -H "$bearer" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')
[ "$second" -gt "$first" ] || fail "outage: the cluster cache did not advance ($first -> $second)"
ok "outage: checkout coordinates through cluster (cache version $first -> $second)"
h upgrade "$release" "$chart" -f "$work/values.yaml" >/dev/null
eventually 60 200 "edge: routing again after its Service is restored" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"

echo "== Flight Control is replaced (Recreate: the directory restarts empty)"
k rollout restart deploy/flight-control >/dev/null
sleep 2
# todo-issues 18: the directory is unreachable, and checkout reaches payments at
# the address it last resolved.
expect 200 "direct: a charge while Flight Control is down, at payments' last known address" \
  POST $checkout_url/api-contracts-consumer/v1/charge -H "$bearer" -H "$json" -d "$charge"
k rollout status deploy/flight-control --timeout=180s >/dev/null
ok "Flight Control replaced"
eventually 90 200 "edge: routes recovered after the replacement" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
start=$(date +%s)
while :; do
  gears=$(listed)
  missing=
  for gear in $workers; do
    case " $gears " in *" $gear "*) ;; *) missing="$missing $gear" ;; esac
  done
  [ -z "$missing" ] && break
  [ $(( $(date +%s) - start )) -lt 60 ] || fail "after 60s the new directory still lacks:$missing"
  sleep 1
done
ok "every worker re-registered with the new directory (after $(( $(date +%s) - start ))s)"

echo "== providers are replaced"
# replace <label> <deployment>: delete its pod, wait until the old one is gone
# and a different one is Ready.
replace() {
  old=$(k get pod -l "app.kubernetes.io/name=$1" -o jsonpath='{.items[0].metadata.uid}')
  k delete pod -l "app.kubernetes.io/name=$1" --wait=true --timeout=120s >/dev/null
  k rollout status "deploy/$2" --timeout=180s >/dev/null
  new=$(k get pod -l "app.kubernetes.io/name=$1" -o jsonpath='{.items[*].metadata.uid}')
  case " $new " in *" $old "*) fail "$1: the old pod $old is still there" ;; esac
  [ -n "$new" ] || fail "$1: no new pod"
  ok "$1 replaced: pod $old gone, $new running"
}
replace payments "$release-payments"
eventually 90 200 "edge: charges reach the new payments pod" \
  POST "$edge/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
replace cluster cluster
eventually 90 200 "direct: checkout coordinates through the new cluster pod" \
  GET $checkout_url/api-contracts-consumer/v1/status -H "$bearer"

echo "== a platform credential is replaced and the retired one refused"
# A ServiceAccount token is a platform credential the API server can revoke:
# deleting the account retires every token minted for it.
k create serviceaccount p4-rotate >/dev/null
retired=$(k create token p4-rotate --audience toolkit-internal --duration 10m)
expect 200 "AuthZ evaluate: a fresh ServiceAccount token" POST $payments_url/authz-resolver/v1/evaluate \
  -H "x-toolkit-internal-token: $retired" -H "$json" -d "$evaluate"
k delete serviceaccount p4-rotate >/dev/null
# The validator caches a review for its TTL (internal_auth_cache_ttl_secs, 30 s).
eventually 90 401 "AuthZ evaluate: the token of a deleted account is refused" \
  POST $payments_url/authz-resolver/v1/evaluate -H "x-toolkit-internal-token: $retired" -H "$json" -d "$evaluate"
k create serviceaccount p4-rotate >/dev/null
replacement=$(k create token p4-rotate --audience toolkit-internal --duration 10m)
expect 200 "AuthZ evaluate: the replacement credential" POST $payments_url/authz-resolver/v1/evaluate \
  -H "x-toolkit-internal-token: $replacement" -H "$json" -d "$evaluate"
expect 401 "AuthZ evaluate: the retired one stays refused" POST $payments_url/authz-resolver/v1/evaluate \
  -H "x-toolkit-internal-token: $retired" -H "$json" -d "$evaluate"

echo "== the same gears as the embedded all-in-one production binary (A20, G18)"
cargo run -q -p gearbox-cli -- generate --root "$gears_rust" --source-id gears-rust \
  --product products/integration-demo/product.gdl --profile all-in-one >/dev/null
aio=.gearbox/integration-demo/all-in-one
if grep -q 'mode = "remote"' "$aio/product.lock"; then fail "an embedded binding is remote"; fi
ok "embedded: every binding is local -- inter-gear calls stay in-process"
if grep -q 'internal_auth' "$aio/config/integration-demo.yaml"; then fail "embedded config carries platform auth"; fi
ok "embedded: no platform authenticator configured"
# Gearbox builds no image for an embedded profile; this one follows the
# generated worker Dockerfile.
sed 's#integration-demo/kind/#integration-demo/all-in-one/#' \
  .gearbox/integration-demo/kind/docker/checkout/Dockerfile.dockerignore >"$work/Dockerfile.dockerignore"
cat >"$work/Dockerfile" <<EOF
FROM rust:1.97.0-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
WORKDIR /src/$(basename "$repo")/$aio
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/$(basename "$repo")/$aio/target,sharing=locked \
    cargo build --release --package gbx-integration-demo \
 && cp target/release/gbx-integration-demo /usr/local/bin/gbx-integration-demo
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* && useradd --uid 65532 --system --no-create-home app \
 && mkdir -p /var/lib/gearbox && chown 65532:65532 /var/lib/gearbox
COPY --from=build /usr/local/bin/gbx-integration-demo /usr/local/bin/gbx-integration-demo
USER 65532
EOF
docker build -f "$work/Dockerfile" -t gbx-integration-demo:all-in-one "$repo/.." >"$work/build-aio.log" 2>&1 \
  || { tail -20 "$work/build-aio.log"; fail "the embedded image did not build"; }
if [ -n "${LOAD_NODE:-}" ]; then
  docker image save --platform "linux/${LOAD_ARCH:-arm64}" --output "$work/image.tar" gbx-integration-demo:all-in-one
  docker exec -i "$LOAD_NODE" ctr --namespace k8s.io images import \
    --platform "linux/${LOAD_ARCH:-arm64}" --digests - <"$work/image.tar" >/dev/null
  rm -f "$work/image.tar"
fi
k create configmap all-in-one-config --from-file=integration-demo.yaml="$aio/config/integration-demo.yaml" >/dev/null
k apply -f - >/dev/null <<EOF
apiVersion: v1
kind: Pod
metadata: {name: all-in-one, labels: {app: all-in-one}}
spec:
  automountServiceAccountToken: false
  containers:
    - name: all-in-one
      image: gbx-integration-demo:all-in-one
      imagePullPolicy: IfNotPresent
      args: [gbx-integration-demo, --config, /etc/gearbox/integration-demo.yaml]
      env:
        - {name: HOME, value: /var/lib/gearbox}
        # The generated listener is loopback; the probe is another pod.
        - {name: APP__GEARS__API_GATEWAY__CONFIG__BIND_ADDR, value: "0.0.0.0:8087"}
      ports: [{containerPort: 8087}]
      readinessProbe: {tcpSocket: {port: 8087}, periodSeconds: 2}
      volumeMounts:
        - {name: config, mountPath: /etc/gearbox, readOnly: true}
        - {name: oidc-ca, mountPath: /app/oidc-ca, readOnly: true}
        - {name: home, mountPath: /var/lib/gearbox}
  volumes:
    - {name: config, configMap: {name: all-in-one-config}}
    - {name: oidc-ca, configMap: {name: idp-ca}}
    - {name: home, emptyDir: {}}
---
apiVersion: v1
kind: Service
metadata: {name: all-in-one}
spec:
  selector: {app: all-in-one}
  ports: [{port: 8087}]
EOF
k wait --for=condition=Ready pod/all-in-one --timeout=180s >/dev/null || {
  k logs all-in-one --tail=30; fail "the embedded binary did not start"
}
ok "embedded: the all-in-one binary starts without a platform authenticator"
aio_url=http://all-in-one:8087
eventually 30 200 "embedded: the exposed charge, provider called in-process" \
  POST "$aio_url/api-contracts-consumer/v1/charge" -H "$bearer" -H "$json" -d "$charge"
expect 401 "embedded: charge without a token" POST "$aio_url/api-contracts-consumer/v1/charge" -H "$json" -d "$charge"
expect 200 "embedded: a forged platform header is stripped" \
  POST "$aio_url/api-contracts-consumer/v1/charge" -H "$bearer" -H "x-toolkit-internal-token: forged" -H "$json" -d "$charge"
internal_absent "$aio_url" embedded
paths=$(body "$aio_url/openapi.json" | python3 -c 'import json,sys; print(" ".join(sorted(json.load(sys.stdin).get("paths", {}))))')
[ "$paths" = "/api-contracts-consumer/v1/charge" ] || fail "embedded OpenAPI lists: $paths"
ok "embedded OpenAPI describes exactly the exposed operation"

echo "$pass checks passed"

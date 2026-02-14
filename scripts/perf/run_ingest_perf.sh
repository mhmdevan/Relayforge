#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${ROOT_DIR}/artifacts/perf"
K6_SCRIPT="${ROOT_DIR}/performance/k6/webhook_ingest.js"
SUMMARY_JSON="${OUT_DIR}/k6-summary.json"
REPORT_MD="${OUT_DIR}/latest-report.md"
API_LOG="${OUT_DIR}/api.log"
RESOURCE_CSV="${OUT_DIR}/api-resource-samples.csv"

RATE="${RATE:-300}"
DURATION="${DURATION:-20s}"
PREALLOCATED_VUS="${PREALLOCATED_VUS:-120}"
MAX_VUS="${MAX_VUS:-400}"
FILLER_BYTES="${FILLER_BYTES:-512}"
WEBHOOK_SECRET="${WEBHOOK_SECRET:-dev-generic-secret}"
K6_IMAGE="${K6_IMAGE:-grafana/k6:0.49.0}"
SKIP_DOCKER_POSTGRES="${SKIP_DOCKER_POSTGRES:-0}"

mkdir -p "${OUT_DIR}"
: > "${RESOURCE_CSV}"

if [[ ! -f "${ROOT_DIR}/.env" ]]; then
  cp "${ROOT_DIR}/.env.example" "${ROOT_DIR}/.env"
fi

cleanup() {
  if [[ -n "${SAMPLER_PID:-}" ]]; then
    kill "${SAMPLER_PID}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${API_PID:-}" ]]; then
    kill "${API_PID}" >/dev/null 2>&1 || true
    wait "${API_PID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

cd "${ROOT_DIR}"

docker_postgres_started=0
if [[ "${SKIP_DOCKER_POSTGRES}" != "1" ]]; then
  if docker compose up -d postgres >/dev/null 2>&1; then
    docker_postgres_started=1
  else
    echo "warning: failed to start docker postgres; falling back to existing DATABASE__URL target" >&2
  fi
fi

if [[ "${docker_postgres_started}" -eq 1 ]]; then
  for _ in {1..40}; do
    if docker compose exec -T postgres pg_isready -U app -d event_gateway >/dev/null 2>&1; then
      postgres_ready=1
      break
    fi
    sleep 1
  done
  if [[ "${postgres_ready:-0}" -ne 1 ]]; then
    echo "postgres container did not become ready in time" >&2
    exit 1
  fi
fi

export HARDENING__INGRESS_RATE_LIMIT_PER_SECOND="${HARDENING__INGRESS_RATE_LIMIT_PER_SECOND:-50000}"
export HARDENING__INGRESS_BURST="${HARDENING__INGRESS_BURST:-100000}"
export DATABASE__MAX_CONNECTIONS="${DATABASE__MAX_CONNECTIONS:-60}"
export LOG__FILTER="${LOG__FILTER:-warn,sqlx=warn,tower_http=warn}"

cargo run -p api --bin api >"${API_LOG}" 2>&1 &
API_PID=$!

for _ in {1..60}; do
  if curl -sSf "http://127.0.0.1:8080/healthz" >/dev/null; then
    api_ready=1
    break
  fi
  sleep 1
done
if [[ "${api_ready:-0}" -ne 1 ]]; then
  echo "api did not become healthy in time" >&2
  exit 1
fi

(
  while kill -0 "${API_PID}" >/dev/null 2>&1; do
    ps -o %cpu=,rss= -p "${API_PID}" | awk -v ts="$(date +%s)" 'NF==2 {printf "%s,%s,%s\n", ts, $1, $2}' >> "${RESOURCE_CSV}"
    sleep 1
  done
) &
SAMPLER_PID=$!

k6_exit_code=0

if command -v k6 >/dev/null 2>&1; then
  BASE_URL="${BASE_URL:-http://127.0.0.1:8080}" \
  WEBHOOK_SECRET="${WEBHOOK_SECRET}" \
  RATE="${RATE}" \
  DURATION="${DURATION}" \
  PREALLOCATED_VUS="${PREALLOCATED_VUS}" \
  MAX_VUS="${MAX_VUS}" \
  FILLER_BYTES="${FILLER_BYTES}" \
  k6 run --summary-export "${SUMMARY_JSON}" "${K6_SCRIPT}" || k6_exit_code=$?
else
  BASE_URL="${BASE_URL:-http://host.docker.internal:8080}"
  docker run --rm \
    --add-host=host.docker.internal:host-gateway \
    -v "${ROOT_DIR}/performance/k6:/scripts" \
    -v "${OUT_DIR}:/out" \
    -e BASE_URL="${BASE_URL}" \
    -e WEBHOOK_SECRET="${WEBHOOK_SECRET}" \
    -e RATE="${RATE}" \
    -e DURATION="${DURATION}" \
    -e PREALLOCATED_VUS="${PREALLOCATED_VUS}" \
    -e MAX_VUS="${MAX_VUS}" \
    -e FILLER_BYTES="${FILLER_BYTES}" \
    "${K6_IMAGE}" run --summary-export /out/k6-summary.json /scripts/webhook_ingest.js || k6_exit_code=$?
fi

kill "${SAMPLER_PID}" >/dev/null 2>&1 || true
wait "${SAMPLER_PID}" >/dev/null 2>&1 || true
SAMPLER_PID=""

throughput_req_s="$(jq -r '(.metrics.http_reqs.values.rate // .metrics.http_reqs.rate // 0)' "${SUMMARY_JSON}")"
p95_ms="$(jq -r '(.metrics.http_req_duration.values["p(95)"] // .metrics.http_req_duration["p(95)"] // 0)' "${SUMMARY_JSON}")"
p99_ms="$(jq -r '(.metrics.http_req_duration.values["p(99)"] // .metrics.http_req_duration["p(99)"] // 0)' "${SUMMARY_JSON}")"
error_rate_pct="$(jq -r '((.metrics.http_req_failed.values.rate // .metrics.http_req_failed.value // 0) * 100)' "${SUMMARY_JSON}")"

cpu_peak_pct="$(awk -F, 'BEGIN{m=0} {if ($2+0>m) m=$2+0} END{printf "%.2f", m}' "${RESOURCE_CSV}")"
rss_peak_kib="$(awk -F, 'BEGIN{m=0} {if ($3+0>m) m=$3+0} END{printf "%.0f", m}' "${RESOURCE_CSV}")"
rss_peak_mib="$(awk -v kib="${rss_peak_kib:-0}" 'BEGIN{printf "%.2f", kib/1024}')"

now_utc="$(date -u +"%Y-%m-%d %H:%M:%S UTC")"

cat > "${REPORT_MD}" <<REPORT
# Performance Proof Report

- Generated at: ${now_utc}
- Scenario: constant-arrival-rate webhook ingest
- Rate: ${RATE} req/s
- Duration: ${DURATION}
- Payload filler bytes: ${FILLER_BYTES}

| Metric | Value |
|---|---|
| Throughput (req/s) | ${throughput_req_s} |
| p95 latency (ms) | ${p95_ms} |
| p99 latency (ms) | ${p99_ms} |
| Error rate (%) | ${error_rate_pct} |
| API peak CPU (%) | ${cpu_peak_pct} |
| API peak RAM (MiB) | ${rss_peak_mib} |

## Reproduce

\`RATE=${RATE} DURATION=${DURATION} ./scripts/perf/run_ingest_perf.sh\`
REPORT

echo "Report written to ${REPORT_MD}"
cat "${REPORT_MD}"
exit "${k6_exit_code}"

# Relayforge SLOs

This document defines operational SLOs, their SLIs, and alert thresholds.

## SLO 1: Webhook Ingest Availability

- Objective: `99.9%` successful ingest responses per 30-day window
- SLI: HTTP success ratio for `POST /webhooks/:source`
- Success definition: non-`5xx` response
- PromQL:

```promql
1 - (
  sum(rate(http_requests_total{route="/webhooks/:source",method="POST",status=~"5.."}[30d]))
  /
  clamp_min(sum(rate(http_requests_total{route="/webhooks/:source",method="POST"}[30d])), 1)
)
```

- Error budget: `0.1%`
- Alerts:
  - `RelayforgeWebhookAvailabilitySLOBreached`

## SLO 2: Webhook Ingest Latency

- Objective: p95 latency below `2s` over rolling 5m windows
- SLI: `http_request_duration_seconds` histogram, filtered by webhook route/method
- PromQL:

```promql
histogram_quantile(
  0.95,
  sum by (le) (
    rate(http_request_duration_seconds_bucket{route="/webhooks/:source",method="POST"}[5m])
  )
)
```

- Alerts:
  - `RelayforgeWebhookLatencyP95High`

## SLO 3: Worker Processing Success

- Objective: success ratio above `99%` over 30-day window
- SLI: `jobs_processed_total{status="succeeded"}` vs all processed statuses
- PromQL:

```promql
sum(rate(jobs_processed_total{status="succeeded"}[30d]))
/
clamp_min(sum(rate(jobs_processed_total[30d])), 1)
```

- Alerts:
  - `RelayforgeWorkerSuccessRatioLow`
  - `RelayforgeDLQSpike`
  - `RelayforgeWorkerRetryStorm`

## SLO 4: Delivery Freshness (Outbox)

- Objective: outbox backlog stays below `1000` pending messages
- SLI: `outbox_pending_count`
- PromQL:

```promql
outbox_pending_count
```

- Alerts:
  - `RelayforgeOutboxBacklogHigh`

## Alert Rule Source

- Prometheus rules: `observability/prometheus/alerts.yml`
- Runbook: `observability/runbook.md`

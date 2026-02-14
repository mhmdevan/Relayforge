# Relayforge Operational Runbook

This runbook maps directly to the alert names in `observability/prometheus/alerts.yml`.

## Shared Triage Checklist

1. Confirm health and readiness:
   - `curl -sS http://127.0.0.1:8080/healthz`
   - `curl -sS http://127.0.0.1:8080/readyz`
2. Check current metrics snapshot:
   - `curl -sS http://127.0.0.1:8080/metrics > /tmp/api.metrics`
   - `curl -sS http://127.0.0.1:9091/metrics > /tmp/worker.metrics`
3. Inspect recent logs with correlation/trace fields:
   - `docker compose logs --since=30m api worker`
4. Confirm infra status:
   - `docker compose ps`

## RelayforgeWebhookAvailabilitySLOBreached

Symptoms:
- sustained 5xx ratio above 1% for webhook ingress.

Immediate actions:
1. Validate DB and Rabbit availability (`readyz`, container health checks).
2. Check if a specific provider/source is failing in logs.
3. Check rollout or config drift in secrets/rate limits.

Deep checks:
- Query top failing statuses/routes in Prometheus or Grafana.
- Verify recent migration/deploy changes.
- Validate tenant secret resolution path (DB first, env fallback).

Mitigation:
- Roll back recent release if regression is confirmed.
- Temporarily reduce ingress rate to stabilize downstream dependencies.
- Isolate noisy tenant/source if needed.

Exit criteria:
- Error ratio below threshold for at least 15 minutes.

## RelayforgeWebhookLatencyP95High

Symptoms:
- p95 latency for `POST /webhooks/:source` above 2s.

Immediate actions:
1. Check API CPU/RAM saturation.
2. Inspect Postgres connection pool pressure and slow queries.
3. Inspect outbox backlog and worker lag.

Deep checks:
- Validate DB write latency on `webhook_inbox`, `jobs`, `outbox_messages`.
- Check rate limiting effectiveness under burst traffic.
- Identify high-latency tenant/provider cohorts.

Mitigation:
- Increase DB pool and worker concurrency within safe limits.
- Scale API replicas.
- Reduce payload size limit only if abuse traffic is the source.

Exit criteria:
- p95 back under 2s for at least 15 minutes.

## RelayforgeOutboxBacklogHigh

Symptoms:
- `outbox_pending_count` above 1000 for 15 minutes.

Immediate actions:
1. Check publisher loop logs for publish errors.
2. Check RabbitMQ health and queue depth.
3. Validate worker consumer liveness and prefetch settings.

Deep checks:
- Compare ingest rate vs publish/consume throughput.
- Verify lock/claim behavior for outbox rows.
- Inspect stuck rows (`locked_until`, `next_attempt_at`).

Mitigation:
- Increase publisher/consumer concurrency.
- Add worker instances.
- Drain retry pressure before normal traffic if retry storm is active.

Exit criteria:
- Backlog trending down and sustained below 1000.

## RelayforgeDLQSpike

Symptoms:
- sudden increase of dead-lettered messages.

Immediate actions:
1. Inspect worker logs for poison payload patterns.
2. Inspect last errors in `jobs.last_error`.
3. Verify provider payload schema/contract changes.

Deep checks:
- Determine if failures are permanent vs transient.
- Validate signature/normalization assumptions per provider.
- Reproduce with captured inbox payloads.

Mitigation:
- Fix parser/connector contract.
- Reprocess dead-lettered jobs after fix using admin endpoint.
- Temporarily reduce max attempts if DLQ storm impacts stability.

Exit criteria:
- No new DLQ spike for 30 minutes and reprocessed jobs succeed.

## RelayforgeWorkerRetryStorm

Symptoms:
- retries growing quickly (`job_retries_total`).

Immediate actions:
1. Identify top transient error causes.
2. Check downstream dependency health and timeouts.
3. Confirm retry TTL configuration is not too aggressive.

Deep checks:
- Inspect retry queue depth and churn.
- Detect retry loops caused by status-gate race or payload constraints.

Mitigation:
- Increase retry TTL to reduce pressure.
- Reduce concurrency if dependency is overloaded.
- Patch transient failure source and reprocess backlog.

Exit criteria:
- Retry rate back to baseline and DLQ growth stopped.

## RelayforgeWorkerSuccessRatioLow

Symptoms:
- worker success ratio below 95% over 15m.

Immediate actions:
1. Split failures by `status` (`retry` vs `dead_lettered`).
2. Inspect correlation IDs and trace chains for failing jobs.
3. Validate DB lookup path for inbox/raw payload.

Deep checks:
- Audit problematic tenants/providers.
- Compare current behavior vs expected failure budget.

Mitigation:
- Address dominant failure mode first (schema, dependency, poison flow).
- Reprocess dead-lettered jobs after deploying fix.

Exit criteria:
- Success ratio above threshold for at least 30 minutes.

# Relayforge Observability

## Runtime endpoints
- API metrics: `GET http://127.0.0.1:8080/metrics`
- Worker metrics: `GET http://127.0.0.1:9091/metrics`

The worker metrics address is configurable with:

```bash
WORKER_METRICS_ADDR=0.0.0.0:9091
```

## OpenTelemetry trace propagation

Relayforge propagates W3C tracing headers end-to-end:

- inbound HTTP headers: `traceparent`, `tracestate`, `baggage`
- API ingest span -> DB/outbox payload (`trace_context`)
- publisher span -> RabbitMQ message payload (`trace_context`)
- consumer span -> retry/dead-letter handling

Environment:

```bash
OTEL__ENABLED=true
OTEL__EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317
OTEL__EXPORTER_OTLP_TIMEOUT_MS=3000
```

## Start observability stack

```bash
docker compose up -d prometheus grafana alertmanager otel-collector
```

Preconfigured local endpoints:
- Prometheus: <http://127.0.0.1:9090>
- Grafana: <http://127.0.0.1:3000> (default login: `admin` / `admin`)
- Alertmanager: <http://127.0.0.1:9093>

## Included dashboard

Grafana auto-loads the dashboard:
- `Relayforge Overview`

It visualizes:
- `http_requests_total`
- `http_request_duration_seconds`
- `jobs_created_total`
- `jobs_processed_total`
- `job_processing_duration_seconds`
- `job_retries_total`
- `dlq_messages_total`
- `outbox_pending_count`

## SLOs, alerts, runbook

- SLO document: `observability/slo.md`
- Prometheus alert rules: `observability/prometheus/alerts.yml`
- Operational runbook: `observability/runbook.md`

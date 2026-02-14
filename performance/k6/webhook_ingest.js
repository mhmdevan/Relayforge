import http from 'k6/http';
import { check } from 'k6';
import exec from 'k6/execution';
import { hmac } from 'k6/crypto';

const BASE_URL = __ENV.BASE_URL || 'http://127.0.0.1:8080';
const WEBHOOK_SECRET = __ENV.WEBHOOK_SECRET || 'dev-generic-secret';
const RATE = Number(__ENV.RATE || 1200);
const DURATION = __ENV.DURATION || '30s';
const PREALLOCATED_VUS = Number(__ENV.PREALLOCATED_VUS || 200);
const MAX_VUS = Number(__ENV.MAX_VUS || 1200);
const FILLER = 'x'.repeat(Number(__ENV.FILLER_BYTES || 512));

export const options = {
  discardResponseBodies: true,
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(90)', 'p(95)', 'p(99)'],
  thresholds: {
    http_req_failed: ['rate<0.01'],
    http_req_duration: ['p(95)<3000', 'p(99)<3500'],
  },
  scenarios: {
    webhook_ingest: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: PREALLOCATED_VUS,
      maxVUs: MAX_VUS,
    },
  },
};

export default function () {
  const iter = exec.scenario.iterationInTest;
  const vu = exec.vu.idInTest;
  const eventId = `k6-${vu}-${iter}`;

  const payload = JSON.stringify({
    event: 'perf.webhook.received',
    data: {
      vu,
      iter,
      filler: FILLER,
    },
  });

  const signature = hmac('sha256', WEBHOOK_SECRET, payload, 'hex');
  const res = http.post(`${BASE_URL}/webhooks/generic`, payload, {
    headers: {
      'content-type': 'application/json',
      'x-signature': signature,
      'x-event-id': eventId,
      'x-event-type': 'perf.webhook.received',
      'x-correlation-id': eventId,
      'user-agent': 'k6-performance-proof/1.0',
    },
    tags: {
      endpoint: 'webhook_ingest',
    },
  });

  check(res, {
    'status is 202': (r) => r.status === 202,
  });
}

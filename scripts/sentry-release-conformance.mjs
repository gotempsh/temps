// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official @sentry/node 11.8.0, c6492ef78c7bfb97931e43075763ce84ee8fda7e.
// Synthetic SDK traffic; transport output is reduced to status codes.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const [entry, run, phase] = process.argv.slice(2);
const S = await import(pathToFileURL(entry).href);
assert.equal(S.SDK_VERSION, '11.8.0');
const statuses = [];
S.init({ dsn: process.env.SENTRY_DSN, defaultIntegrations: false,
  dataCollection: { userInfo: false }, sendClientReports: false, enableLogs: true,
  tracesSampleRate: 1, release: run, environment: 'synthetic-test', serverName: 'synthetic.example',
  transport(options) {
    const transport = S.makeNodeTransport(options);
    return { send: envelope => transport.send(envelope).then(result => { statuses.push(result.statusCode); return result; }), flush: timeout => transport.flush(timeout) };
  },
});
if (phase === 'late') {
  S.logger.info('Synthetic late release log', { 'synthetic.run': run });
  assert.ok(await S.flush(10000)); await S.close(10000);
  assert.ok(statuses.length > 0 && statuses.every(value => value === 200));
  console.log(JSON.stringify({ sdk:S.SDK_VERSION, late:true }));
  process.exit(0);
}
let traceId;
await S.startSpan({ name: 'Synthetic release request', op: 'test.request' }, async span => {
  traceId = span.spanContext().traceId;
  S.logger.info('Synthetic release log', { 'synthetic.run': run });
  S.metrics.count('synthetic.release.requests', 2, { attributes: { 'synthetic.run': run } });
  S.metrics.gauge('synthetic.release.queue', 7, { attributes: { 'synthetic.run': run } });
  S.metrics.distribution('synthetic.release.duration', 12.5, { unit: 'millisecond', attributes: { 'synthetic.run': run } });
  const eventId = S.captureMessage('Synthetic release message');
  S.captureFeedback({ message: 'Synthetic release feedback', associatedEventId: eventId });
  S.captureCheckIn({ monitorSlug: 'synthetic-release', status: 'ok', duration: 0.01 }, { schedule: { type: 'interval', value: 1, unit: 'hour' } });
});
assert.ok(await S.flush(10000));
await S.close(10000);
assert.ok(statuses.length > 0 && statuses.every(value => value === 200));
console.log(JSON.stringify({ sdk: S.SDK_VERSION, trace_id: traceId, deliveries: statuses.length }));

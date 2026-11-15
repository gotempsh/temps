// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Official Node 11.8.0: actual SDK beforeSend/log filtering and outcome flush.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const [entry, run] = process.argv.slice(2);
const S = await import(pathToFileURL(entry).href);
assert.equal(S.SDK_VERSION, '11.8.0');
const reports = [], deliveries = [];
function transport(options) {
  const transport = S.makeNodeTransport(options);
  return {
    send(envelope) {
      for (const [header, payload] of envelope[1]) if (header.type === 'client_report') {
        assert.equal(envelope[0].event_id, undefined, 'Pinned SDK report unexpectedly gained an envelope identity');
        assert.equal(payload.event_id, undefined, 'Pinned SDK report unexpectedly gained a report identity');
        reports.push(payload);
      }
      return transport.send(envelope).then(result => { deliveries.push(result.statusCode); return result; });
    }, flush: timeout => transport.flush(timeout),
  };
}
S.init({ dsn: process.env.SENTRY_DSN, defaultIntegrations: false, sendClientReports: true,
  enableLogs: true,
  beforeSend(event) { return event.message === 'Synthetic accepted outcome companion' ? event : null; },
  beforeSendLog() { return null; },
  transport,
});
S.setTag('synthetic.run', run);
for (let index = 0; index < 3; index++) S.captureException(new Error('Synthetic filtered outcome'));
S.logger.info('Synthetic filtered log', { 'synthetic.run': run });
// The pinned SDK emits log_item for beforeSendLog but does not yet emit its
// byte companion automatically. Exercise those wire categories through the
// official client's public discard counter without claiming automatic bytes.
S.getClient().recordDroppedEvent('buffer_overflow', 'log_item', 5);
S.getClient().recordDroppedEvent('buffer_overflow', 'log_byte', 12400);
const eventId = S.captureMessage('Synthetic accepted outcome companion');
assert.ok(await S.flush(10000));
assert.ok(await S.close(10000));
// Two independent official SDK clients can report the same reason/category/
// quantity with no report or envelope ID. Both reports must count. A payload
// hash is not a safe retry identity for this protocol.
for (let index = 0; index < 2; index++) {
  const client = new S.NodeClient({ dsn: process.env.SENTRY_DSN, integrations: [],
    sendClientReports: true, transport });
  client.recordDroppedEvent('sample_rate', 'span', 7);
  assert.ok(await client.flush(10000));
  assert.ok(await client.close(10000));
}
assert.ok(deliveries.length && deliveries.every(status => status === 200));
const totals = {};
for (const report of reports) for (const entry of report.discarded_events) {
  const key = entry.reason + '/' + entry.category;
  totals[key] = (totals[key] || 0) + entry.quantity;
}
assert.equal(totals['before_send/error'], 3);
assert.equal(totals['before_send/log_item'], 1);
assert.equal(totals['buffer_overflow/log_item'], 5);
assert.equal(totals['buffer_overflow/log_byte'], 12400);
assert.equal(totals['sample_rate/span'], 14);
console.log(JSON.stringify({ sdk: S.SDK_VERSION, event_id: eventId, totals,
  report_envelopes: reports.length, independent_clients: 2, stable_report_identity: false }));

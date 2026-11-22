// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Synthetic check-ins from official @sentry/node 10.17.0 or 11.8.0.
// Source revisions: eb6e2946eb45583dd4c02ec92f8912e5bad51943 and
// c6492ef78c7bfb97931e43075763ce84ee8fda7e. Create the monitor through
// Temps' authenticated native API first; this runner receives only its DSN.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const [entry, dsn, slug, mode = 'success', checkInId] = process.argv.slice(2);
assert.ok(entry && dsn && slug);
assert.ok(['success', 'error', 'start', 'complete', 'stale-config'].includes(mode));
const Sentry = await import(pathToFileURL(entry).href);
assert.ok(['10.17.0', '11.8.0'].includes(Sentry.SDK_VERSION));
const deliveries = [];
Sentry.init({
  dsn, defaultIntegrations: false, sendClientReports: false,
  release: 'synthetic-cron-release', environment: 'synthetic-cron-test',
  transport(options) {
    const transport = Sentry.makeNodeTransport(options);
    return {
      send(envelope) {
        return transport.send(envelope).then(response => {
          deliveries.push(response.statusCode);
          return response;
        });
      },
      flush: timeout => transport.flush(timeout),
    };
  },
});
let id;
if (mode === 'start' || mode === 'complete') {
  id = Sentry.captureCheckIn({ monitorSlug: slug, status: mode === 'start' ? 'in_progress' : 'ok', ...(checkInId ? { checkInId } : {}) });
} else {
  const staleConfig = mode === 'stale-config' ? { schedule: { type: 'interval', value: 7, unit: 'day' }, maxRuntime: 999 } : undefined;
  try {
    await Sentry.withMonitor(slug, async () => {
      if (mode === 'error') throw new Error('Synthetic scheduled job failure');
      return 1;
    }, staleConfig);
  } catch (error) {
    assert.equal(mode, 'error');
    assert.equal(error.message, 'Synthetic scheduled job failure');
  }
}
assert.equal(await Sentry.flush(10000), true);
await Sentry.close(10000);
assert.ok(deliveries.length > 0);
assert.ok(deliveries.every(status => status === 200), 'SDK delivery was rejected');
process.stdout.write(JSON.stringify({ sdk: Sentry.SDK_VERSION, slug, mode, check_in_id: id, deliveries }) + '\n');

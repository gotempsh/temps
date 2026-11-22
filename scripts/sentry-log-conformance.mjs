// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Reproducible synthetic SDK traffic. Install @sentry/node@10.17.0 or @11.8.0
// in a temporary directory; supply its resolved entry point, DSN and mode.
// Node modes: direct, tunnel, mixed, errors, errors-tunnel, cron,
// cron-tunnel, correlated (logs linked to a real SDK span), and signals
// (one envelope mixing logs, error and check-in).
// Source pins: 10.17.0 eb6e2946eb45583dd4c02ec92f8912e5bad51943;
// 11.8.0 c6492ef78c7bfb97931e43075763ce84ee8fda7e.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import * as http from 'node:http';
import * as https from 'node:https';
import { createRequire } from 'node:module';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [entry, dsn, mode = 'direct', tunnel] = process.argv.slice(2);
// Exercise the Node SDK in Node itself; Bun is only needed for browser bundling.
if (!mode.startsWith('browser') && typeof Bun !== 'undefined') {
  const result = spawnSync('node', [process.argv[1], ...process.argv.slice(2)], { stdio: 'inherit' });
  if (result.error) throw result.error;
  process.exit(result.status ?? 1);
}
if (mode.startsWith('browser')) {
  // Supply @sentry/browser@11.8.0's entry point from a temporary installation
  // containing playwright@1.64.1. Run its actual browser build in Chromium.
  const require = createRequire(pathToFileURL(entry));
  const { chromium } = await import(pathToFileURL(require.resolve('playwright')).href);
  const directory = await mkdtemp(join(tmpdir(), 'temps-sentry-browser-'));
  const browser = await chromium.launch({ headless: true });
  try {
    const wrapper = join(directory, 'entry.mjs');
    await writeFile(wrapper, `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nimport * as SDK from ${JSON.stringify(entry)}; window.SyntheticSentry = SDK;`);
    const build = await Bun.build({ entrypoints: [wrapper], target: 'browser', format: 'iife' });
    assert.equal(build.success, true);
    const page = await browser.newPage();
    const pageUrl = new URL(tunnel).origin + '/synthetic-conformance';
    await page.route(pageUrl, route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Synthetic SDK conformance</title>' }));
    await page.goto(pageUrl);
    await page.addScriptTag({ content: await build.outputs[0].text() });
    const result = await page.evaluate(async ({ dsn, tunnel, mode }) => {
      const SDK = window.SyntheticSentry;
      const deliveries = [];
      const records = [];
      SDK.init({
        dsn, tunnel: mode === 'browser-tunnel' ? tunnel : undefined,
        // Explicitly exercise both opt-in and opt-out in official v2 batches.
        enableLogs: true, defaultIntegrations: false,
        dataCollection: { userInfo: mode === 'browser-auto' },
        sendClientReports: false, release: 'synthetic-release-1', environment: 'synthetic-test',
        beforeSendLog(log) { return log; },
        transport(options) {
          const transport = SDK.makeFetchTransport(options);
          return {
            send(envelope) {
              records.push(...envelope[1].filter(([h]) => h.type === 'log').flatMap(([, batch]) => batch.items));
              return transport.send(envelope).then(response => { deliveries.push(response.statusCode); return response; });
            },
            flush: timeout => transport.flush(timeout),
          };
        },
      });
      SDK.logger.info(SDK.logger.fmt`Synthetic item ${7}`, { 'synthetic.mode': mode, 'service.name': 'synthetic-service' });
      SDK.logger.warn('Synthetic warning', { 'synthetic.mode': mode });
      const flushed = await SDK.flush(10000);
      await SDK.close(10000);
      return { sdk: SDK.SDK_VERSION, mode, logs: records.length, trace_id: records[0]?.trace_id, deliveries, flushed };
    }, { dsn, tunnel, mode });
    assert.equal(result.sdk, '11.8.0');
    assert.equal(result.logs, 2);
    assert.equal(result.flushed, true);
    assert.ok(result.trace_id);
    process.stdout.write(JSON.stringify(result) + '\n');
  } finally {
    await browser.close();
    await rm(directory, { recursive: true });
  }
  process.exit(0);
}
const Sentry = await import(pathToFileURL(entry).href);
assert.ok(['10.17.0', '11.8.0'].includes(Sentry.SDK_VERSION));
const captured = [];
const deliveryFailures = [];
const outcomes = [];
const deferred = mode === 'mixed' || mode === 'signals';
const errorsOnly = mode === 'errors' || mode === 'errors-tunnel';
const cronOnly = mode === 'cron' || mode === 'cron-tunnel';
let transport;
Sentry.init({
  dsn,
  tunnel: mode === 'tunnel' || mode.endsWith('-tunnel') ? tunnel : undefined,
  enableLogs: true,
  tracesSampleRate: mode === 'correlated' ? 1 : undefined,
  defaultIntegrations: false,
  sendClientReports: false,
  release: 'synthetic-release-1',
  environment: 'synthetic-test',
  beforeSendLog(log) {
    log.attributes = { ...log.attributes, 'server.address': 'synthetic.example' };
    return log;
  },
  beforeSend(event) {
    event.server_name = 'synthetic.example';
    for (const exception of event.exception?.values ?? []) {
      for (const frame of exception.stacktrace?.frames ?? []) {
        frame.filename = 'synthetic:///application.mjs';
        if (frame.abs_path) frame.abs_path = 'synthetic:///application.mjs';
      }
    }
    return event;
  },
  beforeSendTransaction(event) {
    event.server_name = 'synthetic.example';
    return event;
  },
  transport(options) {
    const nativeHttp = new URL(options.url).protocol === 'https:' ? https : http;
    // Observe response counts without replacing SDK framing, gzip or transport.
    transport = Sentry.makeNodeTransport({ ...options, httpModule: {
      ...nativeHttp,
      request(requestOptions, callback) {
        return nativeHttp.request(requestOptions, response => {
          outcomes.push({
            status: response.statusCode,
            errors: Number(response.headers['x-temps-sentry-errors-stored'] ?? 0),
            logs: Number(response.headers['x-temps-sentry-logs-stored'] ?? 0),
            check_ins: Number(response.headers['x-temps-sentry-check-ins-stored'] ?? 0),
            pending_configuration: Number(response.headers['x-temps-sentry-check-ins-pending-config'] ?? 0),
            unsupported: Number(response.headers['x-temps-sentry-unsupported-items'] ?? 0),
            types: response.headers['x-temps-sentry-unsupported-types'] ?? '',
            gzip: requestOptions.headers?.['content-encoding'] === 'gzip',
          });
          callback(response);
        });
      },
    } });
    return {
      send(envelope) {
        captured.push(envelope);
        if (deferred) return Promise.resolve({ statusCode: 200 });
        return transport.send(envelope).then(
          response => {
            if (response.statusCode !== 200) deliveryFailures.push({ status: response.statusCode, items: envelope[1].map(([header, item]) => ({ type: header.type, slug: item.monitor_slug, status: item.status, configured: !!item.monitor_config })) });
            return response;
          },
          error => { deliveryFailures.push(error.code ?? 'transport failure'); throw error; },
        );
      },
      flush: timeout => transport.flush(timeout),
    };
  },
});

if (mode === 'correlated') {
  let traceId, spanId;
  await Sentry.startSpan({ name: 'Synthetic Node correlated request', op: 'test.request' }, async span => {
    ({ traceId, spanId } = span.spanContext());
    Sentry.withActiveSpan(span, () => {
      Sentry.logger.info('Synthetic Node correlated info', { 'service.name': 'synthetic-service' });
      Sentry.logger.warn('Synthetic Node correlated warning', { 'service.name': 'synthetic-service' });
    });
  });
  assert.equal(await Sentry.flush(10000), true);
  await Sentry.close(10000);
  const logs = captured.flatMap(([, items]) => items.filter(([header]) => header.type === 'log').flatMap(([, batch]) => batch.items));
  assert.equal(logs.length, 2);
  for (const log of logs) {
    assert.equal(log.trace_id, traceId);
    const parent = log.attributes['sentry.trace.parent_span_id']?.value;
    if (parent !== undefined) assert.equal(parent, spanId);
  }
  // Node 10.17 reads the core scope for this attribute, while its active span
  // lives in OpenTelemetry context. It supplies trace_id but no parent span.
  // Node 11.8 reads the active span and supplies the official typed attribute.
  const parentSpanEmitted = logs.every(log => log.attributes['sentry.trace.parent_span_id']?.value === spanId);
  assert.equal(parentSpanEmitted, Sentry.SDK_VERSION === '11.8.0');
  assert.deepEqual(deliveryFailures, []);
  assert.ok(outcomes.every(outcome => outcome.status === 200));
  process.stdout.write(JSON.stringify({ sdk: Sentry.SDK_VERSION, mode, logs: logs.length, trace_id: traceId, span_id: spanId, parent_span_emitted: parentSpanEmitted, deliveries: outcomes.length }) + '\n');
  process.exit(0);
}

const eventIds = [];
if (!errorsOnly && !cronOnly) {
  // Tracing without performance capture supplies the propagation trace ID.
  Sentry.logger.info(Sentry.logger.fmt`Synthetic item ${7}`, {
  'service.name': 'synthetic-service',
  'synthetic.mode': mode,
  'synthetic.integer': 42,
  'synthetic.flag': true,
  });
  Sentry.logger.warn('Synthetic warning', { 'synthetic.mode': mode });
}
if (mode === 'mixed' || mode === 'signals') {
  eventIds.push(Sentry.captureException(new Error('Synthetic mixed error')));
}
if (errorsOnly) {
  Sentry.withScope(scope => {
    scope.setUser({ id: 'synthetic-user', email: 'synthetic@example.invalid' });
    scope.setTag('synthetic.mode', mode);
    scope.setContext('synthetic', { operation: 'synthetic-operation', attempt: 7 });
    scope.setExtra('synthetic.padding', 'x'.repeat(40000));
    scope.setFingerprint(['synthetic-fingerprint']);
    Sentry.addBreadcrumb({ category: 'synthetic', message: 'Synthetic breadcrumb', level: 'info' });
    function captureSyntheticError() {
      return Sentry.captureException(new TypeError('Synthetic repeated failure'));
    }
    for (let i = 0; i < 2; i++) eventIds.push(captureSyntheticError());
    eventIds.push(Sentry.captureMessage('Synthetic SDK message', 'warning'));
  });
}
if (cronOnly) {
  const config = { schedule: { type: 'crontab', value: '0 * * * *' }, timezone: 'UTC', checkinMargin: 1, maxRuntime: 1 };
  const checkInId = Sentry.captureCheckIn({ monitorSlug: 'synthetic-manual', status: 'in_progress' }, config);
  Sentry.captureCheckIn({ checkInId, monitorSlug: 'synthetic-manual', status: 'ok', duration: 0.01 });
  Sentry.captureCheckIn({ monitorSlug: 'synthetic-heartbeat', status: 'error', duration: 0.01 }, config);
  await Sentry.withMonitor('synthetic-success', async () => 7, config);
  await assert.rejects(Sentry.withMonitor('synthetic-failure', async () => {
    throw new Error('Synthetic monitored failure');
  }, config), /Synthetic monitored failure/);
}
if (mode === 'signals') Sentry.captureCheckIn({ monitorSlug: 'synthetic-mixed', status: 'ok' }, { schedule: { type: 'interval', value: 1, unit: 'hour' } });
assert.equal(await Sentry.flush(10000), true);
assert.deepEqual(deliveryFailures, []);
if (deferred) {
  const error = captured.find(([, items]) => items.some(([h]) => h.type === 'event'));
  const logs = captured.flatMap(([, items]) => items.filter(([h]) => h.type === 'log'));
  assert.ok(error);
  assert.equal(logs.length, 1);
  const checkIns = captured.flatMap(([, items]) => items.filter(([h]) => h.type === 'check_in'));
  const response = await transport.send([error[0], [...error[1], ...logs, ...checkIns]]);
  assert.equal(response.statusCode, 200);
  assert.equal(await transport.flush(10000), true);
}
const records = captured.flatMap(([, items]) => items.filter(([h]) => h.type === 'log').flatMap(([, batch]) => batch.items));
const checkIns = captured.flatMap(([, items]) => items.filter(([h]) => h.type === 'check_in').map(([, item]) => item));
const events = captured.flatMap(([, items]) => items.filter(([h]) => h.type === 'event').map(([, item]) => item));
assert.equal(records.length, errorsOnly || cronOnly ? 0 : 2);
assert.ok(records.every(log => log.trace_id));
assert.ok(outcomes.length > 0);
if (errorsOnly) {
  assert.equal(events.length, 3);
}
if (cronOnly) {
  assert.equal(checkIns.length, 7);
  assert.deepEqual(checkIns.map(item => item.status).sort(), ['error', 'error', 'in_progress', 'in_progress', 'in_progress', 'ok', 'ok'].sort());
  assert.ok(checkIns.some(item => item.monitor_config?.schedule?.value === '0 * * * *'));
}
process.stdout.write(JSON.stringify({ sdk: Sentry.SDK_VERSION, runtime: process.version, mode, logs: records.length, events: eventIds, check_ins: checkIns.length, runs: [...new Map(checkIns.map(item => [item.check_in_id, { id: item.check_in_id, slug: item.monitor_slug, status: item.status, duration: item.duration, trace_id: item.contexts?.trace?.trace_id, span_id: item.contexts?.trace?.span_id }])).values()], trace_id: records[0]?.trace_id ?? events[0]?.contexts?.trace?.trace_id, outcomes }) + '\n');
await Sentry.close(10000);

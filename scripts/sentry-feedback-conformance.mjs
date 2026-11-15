// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official SDK transports, synthetic metadata and feedback and retry traffic. No payloads or
// credentials are printed. SDK pins/source revisions live in the native docs.
// Additional feedback baseline: 8.47.0, eac1a8821988618eb12821011be901a12958978f.
// Usage: node <script> <Node SDK entry> <DSN> <run key> [tunnel URL]
// Browser: bun <script> <browser SDK entry> <DSN> <run key> [tunnel URL]
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const [entry, dsn, prefix, tunnel] = process.argv.slice(2);
async function emit({ dsn, prefix, tunnel }) {
  const S = globalThis.SyntheticSentry;
  const browser = typeof document !== "undefined";
  const captured = [], deliveries = [];
  let transport;
  let traceId;
  S.init({
    dsn, tunnel, defaultIntegrations: false, sendClientReports: false,
    dataCollection: browser ? undefined : { userInfo: false }, sendDefaultPii: false,
    tracesSampleRate: browser ? 1 : 0, enableLogs: browser,
    release: 'synthetic-feedback-release', environment: 'synthetic-test',
    ...(browser ? { replaysSessionSampleRate: 1, replaysOnErrorSampleRate: 0,
      integrations: [S.replayIntegration({ maskAllText: true, maskAllInputs: true, minReplayDuration: 0, flushMinDelay: 0 })] } : {}),
    beforeSend(event) { event.server_name = 'synthetic.example'; return event; },
    transport(options) {
      transport = S.makeNodeTransport ? S.makeNodeTransport(options) : S.makeFetchTransport(options);
      return {
        send(envelope) { captured.push(envelope); return transport.send(envelope).then(r => { deliveries.push(r.statusCode); return r; }); },
        flush: timeout => transport.flush(timeout),
      };
    },
  });
  S.setUser({ id: 'synthetic-user', email: 'synthetic@example.invalid' });
  S.setTag('synthetic.run', prefix);
  S.setContext('synthetic', { count: 7, nullable: null });
  const replay = browser ? S.getReplay() : undefined;
  if (browser) {
    await new Promise(resolve => setTimeout(resolve, 300));
    const button = document.querySelector('button');
    button?.click();
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  if (browser) {
    await S.startSpan({ name: 'Synthetic feedback root', op: 'ui.action' }, async root => {
      traceId = root.spanContext().traceId;
      await S.startSpan({ name: 'Synthetic feedback child', op: 'ui.action' }, async () => {
        S.logger.info('Synthetic feedback associated log', { 'synthetic.run': prefix, count: 7, valid: true });
        S.metrics.count('synthetic.feedback.requests', 1, { attributes: { 'synthetic.run': prefix } });
        S.metrics.gauge('synthetic.feedback.queue', 7, { attributes: { 'synthetic.run': prefix } });
        S.metrics.distribution('synthetic.feedback.duration', 12.5, { unit: 'millisecond', attributes: { 'synthetic.run': prefix } });
      });
    });
  }
  const errorId = S.captureMessage('Synthetic feedback associated error');
  const linked = S.captureFeedback({ message: 'Synthetic linked report ' + prefix, name: 'Synthetic user', email: 'synthetic@example.invalid', url: 'https://synthetic.example/checkout', associatedEventId: errorId }, { includeReplay: browser });
  const standalone = S.captureFeedback({ message: 'Synthetic standalone report ' + prefix });
  const replayId = replay?.getReplayId();
  if (replay) { await replay.flush(); await replay.stop(); }
  assertSafe(await S.flush(10000), 'SDK flush failed');
  const original = captured.find(([, items]) => items.some(([h]) => h.type === 'feedback'));
  assertSafe(!!original, 'SDK emitted no feedback');
  await new Promise(resolve => setTimeout(resolve, 100));
  const retried = await transport.send(original);
  deliveries.push(retried.statusCode);
  await S.close(10000);
  assertSafe(deliveries.every(status => status === 200), 'SDK delivery failed: statuses=' + deliveries.join(','));
  return { sdk: S.SDK_VERSION, ids: [linked, standalone], error_id: errorId, replay_id: replayId, trace_id: traceId, deliveries: deliveries.length, retry: true };
  function assertSafe(condition, message) { if (!condition) throw new Error(message); }
}
let result;
if (entry.includes('/browser/')) {
  const require = createRequire(pathToFileURL(entry));
  const { chromium } = await import(pathToFileURL(require.resolve('playwright')).href);
  const directory = await mkdtemp(join(tmpdir(), 'temps-sentry-feedback-'));
  const browser = await chromium.launch({ headless: true });
  try {
    const wrapper = join(directory, 'entry.mjs');
    await writeFile(wrapper, `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nimport * as S from ${JSON.stringify(entry)}; globalThis.SyntheticSentry = S;`);
    const build = await Bun.build({ entrypoints: [wrapper], target: 'browser', format: 'iife' });
    assert.equal(build.success, true);
    const page = await browser.newPage();
    const failures = [];
    page.on('requestfailed', request => failures.push(request.failure()?.errorText ?? 'unknown'));
    const url = new URL(dsn); url.username = ''; url.pathname = '/synthetic-sdk-conformance';
    const html = await readFile(new URL('./fixtures/sentry-landing.html', import.meta.url), 'utf8');
    await page.route(url.href, route => route.fulfill({ contentType: 'text/html', body: html }));
    await page.goto(url.href);
    await page.addScriptTag({ content: await build.outputs[0].text() });
    try { result = await page.evaluate(emit, { dsn, prefix, tunnel }); }
    catch (error) { const stage = String(error.message).includes('SDK retry failed') ? 'retry' : 'flush'; throw new Error('Browser SDK ' + stage + ' request failures=' + failures.slice(0,10).join(',')); }
  } finally { await browser.close(); await rm(directory, { recursive: true }); }
} else {
  globalThis.SyntheticSentry = await import(pathToFileURL(entry).href);
  result = await emit({ dsn, prefix, tunnel });
}
assert.ok(['8.47.0','10.17.0','11.8.0'].includes(result.sdk));
assert.equal(result.ids.length, 2);
process.stdout.write(JSON.stringify(result) + '\n');

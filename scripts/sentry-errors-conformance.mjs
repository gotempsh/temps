// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official SDK transports, synthetic metadata and retry traffic. No payloads or
// credentials are printed. SDK pins/source revisions live in the native docs.
// Usage: node <script> <Node SDK entry> <DSN> <run key> [tunnel URL]
// Browser: bun <script> <browser SDK entry> <DSN> <run key> [tunnel URL]
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const [entry, dsn, prefix, tunnel] = process.argv.slice(2);
async function emit({ dsn, prefix, tunnel }) {
  const S = globalThis.SyntheticSentry;
  const captured = [], deliveries = [], ids = [];
  let transport;
  S.init({
    dsn, tunnel, defaultIntegrations: false, sendClientReports: false,
    integrations: [S.linkedErrorsIntegration()], attachStacktrace: true,
    dataCollection: { userInfo: false }, sendDefaultPii: false,
    release: 'synthetic-release-2', environment: 'synthetic-test',
    beforeSend(event) {
      event.server_name = 'synthetic.example';
      event.request = { url: 'https://synthetic.example/form', method: 'POST',
        headers: { 'User-Agent': 'SyntheticAgent/1', Referer: 'https://synthetic.example/' },
        data: { synthetic: true }, query_string: [['example', 'value']] };
      event.contexts = { ...event.contexts, browser: { name: 'SyntheticBrowser', version: '1' },
        synthetic: { nullable: null, array: [1, true, null], nested: { key: 'value' } } };
      for (const exception of event.exception?.values ?? []) {
        for (const frame of exception.stacktrace?.frames ?? []) {
          frame.filename = 'synthetic:///application.mjs';
          if (frame.abs_path) frame.abs_path = 'synthetic:///application.mjs';
        }
      }
      return event;
    },
    transport(options) {
      transport = S.makeNodeTransport ? S.makeNodeTransport(options) : S.makeFetchTransport(options);
      return {
        send(envelope) { captured.push(envelope); return transport.send(envelope).then(r => { deliveries.push(r.statusCode); return r; }); },
        flush: timeout => transport.flush(timeout),
      };
    },
  });
  S.setUser({ id: 'synthetic-user', email: 'synthetic@example.invalid', segment: 'synthetic' });
  S.setTag('synthetic.run', prefix);
  S.setTag('synthetic.key', "test'value");
  S.setExtra('synthetic', { nullable: null, array: [1, true, null] });
  S.setExtra('synthetic.padding', 'x'.repeat(40000));
  S.addBreadcrumb({ category: 'synthetic', message: 'Synthetic interaction', data: { nullable: null, count: 7 } });
  for (const [index, fingerprint] of [[0,[prefix,'shared']], [1,[prefix,'shared']], [2,[prefix,'split']]]) {
    S.withScope(scope => {
      scope.setFingerprint(fingerprint);
      const cause = new Error('Synthetic inner failure');
      const error = index === 1 ? new TypeError('Synthetic different type', { cause }) : new Error('Synthetic outer failure', { cause });
      ids.push(S.captureException(error));
    });
  }
  S.withScope(scope => { scope.setFingerprint(['{{ default }}', prefix, 'message']); ids.push(S.captureMessage('Synthetic SDK message')); });
  assertSafe(await S.flush(10000).catch(() => { throw new Error('SDK initial flush failed'); }), 'SDK did not flush');
  // Replay an SDK-produced envelope through its own transport/serializer.
  const original = captured.find(([, items]) => items.some(([h]) => h.type === 'event'));
  assertSafe(!!original, 'SDK emitted no errors');
  // A retry happens after the preceding SDK flush and browser keepalive work.
  await new Promise(resolve => setTimeout(resolve, 100));
  const retried = await transport.send(original).catch(() => { throw new Error('SDK retry failed'); });
  deliveries.push(retried.statusCode);
  await S.close(10000);
  assertSafe(deliveries.every(status => status === 200), 'SDK delivery failed: statuses=' + deliveries.join(','));
  return { sdk: S.SDK_VERSION, ids, deliveries: deliveries.length, retry: true };
  function assertSafe(condition, message) { if (!condition) throw new Error(message); }
}
let result;
if (entry.includes('/browser/')) {
  const require = createRequire(pathToFileURL(entry));
  const { chromium } = await import(pathToFileURL(require.resolve('playwright')).href);
  const directory = await mkdtemp(join(tmpdir(), 'temps-sentry-errors-'));
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
    await page.route(url.href, route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Synthetic SDK conformance</title>' }));
    await page.goto(url.href);
    await page.addScriptTag({ content: await build.outputs[0].text() });
    try { result = await page.evaluate(emit, { dsn, prefix, tunnel }); }
    catch (error) { const stage = String(error.message).includes('SDK retry failed') ? 'retry' : 'flush'; throw new Error('Browser SDK ' + stage + ' request failures=' + failures.slice(0,10).join(',')); }
  } finally { await browser.close(); await rm(directory, { recursive: true }); }
} else {
  globalThis.SyntheticSentry = await import(pathToFileURL(entry).href);
  result = await emit({ dsn, prefix, tunnel });
}
assert.ok(['10.17.0','11.8.0'].includes(result.sdk));
assert.equal(result.ids.length, 4);
process.stdout.write(JSON.stringify(result) + '\n');

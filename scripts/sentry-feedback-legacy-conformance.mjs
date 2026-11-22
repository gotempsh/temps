// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official legacy browser SDK transport and synthetic retry traffic. No payloads or
// credentials are printed. SDK pins/source revisions live in the native docs.
// Legacy browser baseline: 7.120.4, 0469cab0f9f8cebed33620a482441ee505be8787.
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
    beforeSend(event) { event.server_name = 'synthetic.example'; return event; },
    transport(options) {
      transport = S.makeFetchTransport(options);
      return {
        send(envelope) { captured.push(envelope); return transport.send(envelope).then(response => { deliveries.push(response.statusCode); return response; }); },
        flush: timeout => transport.flush(timeout),
      };
    },
  });
  for (const label of ['linked', 'second']) {
    const id = S.captureMessage('Synthetic legacy associated ' + label);
    ids.push(id);
    S.captureUserFeedback({ event_id: id, comments: 'Synthetic legacy ' + label + ' report ' + prefix, name: 'Synthetic user', email: 'synthetic@example.invalid' });
  }
  assertSafe(await S.flush(10000), 'SDK flush failed');
  const envelope = captured.find(([, items]) => items.some(([header]) => header.type === 'user_report'));
  assertSafe(!!envelope, 'SDK emitted no legacy feedback');
  await new Promise(resolve => setTimeout(resolve, 100));
  deliveries.push((await transport.send(envelope)).statusCode);
  await S.close(10000);
  assertSafe(deliveries.every(status => status === 200), 'SDK delivery failed');
  return { sdk: S.SDK_VERSION, ids, legacy: true, retry: true };
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
assert.ok(['7.120.4'].includes(result.sdk));
assert.equal(result.ids.length, 2);
process.stdout.write(JSON.stringify(result) + '\n');

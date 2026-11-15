// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Browser-generated CSP and automatic NEL, plus explicit W3C NEL fixtures.
// Playwright 1.64.1 / full Chromium 156.0.8078.4; the separate headless shell
// does not deliver NEL in this scenario. Servers/certificates are temporary.
import assert from 'node:assert/strict';
import { createServer as httpServer } from 'node:http';
import { createServer as httpsServer } from 'node:https';
import { X509Certificate, createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
const [entry, run] = process.argv.slice(2);
const require = createRequire(pathToFileURL(entry));
const { chromium } = require('playwright');
assert.equal(require('playwright/package.json').version, '1.64.1');
const dsn = new URL(process.env.SENTRY_DSN);
const key = dsn.username, project = dsn.pathname.split('/').filter(Boolean).at(-1);
dsn.username = ''; dsn.password = ''; dsn.search = ''; dsn.hash = '';
const endpoint = type => { const url = new URL(dsn); url.pathname = `/api/${project}/${type}/`; url.searchParams.set('sentry_key', key); url.searchParams.set('sentry_release', run); return url.href; };
const csp = endpoint('security'), nel = endpoint('nel');
const directory = await mkdtemp(join(tmpdir(), 'temps-synthetic-browser-reports-'));
let browser, plain, secure;
const automatic = [];
try {
  process.stderr.write('SYNTHETIC_STAGE=browser_fixture\n');
  plain = httpServer((request, response) => {
    response.writeHead(200, { 'Content-Type': 'text/html', 'Content-Security-Policy': `script-src 'none'; report-uri ${csp}` });
    response.end('<!doctype html><title>Synthetic policy</title><script>window.syntheticBlocked=true</script><p>Synthetic CSP violation</p>');
  });
  await new Promise(resolve => plain.listen(0, '127.0.0.1', resolve));
  const sourceUrl = `http://127.0.0.1:${plain.address().port}/${run}`;
  const cert = join(directory, 'cert.pem'), privateKey = join(directory, 'key.pem');
  process.stderr.write('SYNTHETIC_STAGE=certificate\n');
  const generated = spawnSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', privateKey, '-out', cert, '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1'], { encoding: 'utf8', timeout: 30000 });
  assert.equal(generated.status, 0, 'Synthetic TLS setup failed; output suppressed');
  const certificate = await readFile(cert);
  const spki = createHash('sha256').update(new X509Certificate(certificate).publicKey.export({ type: 'spki', format: 'der' })).digest('base64');
  secure = httpsServer({ key: await readFile(privateKey), cert: certificate }, async (request, response) => {
    if (request.url === '/report') {
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 65536) { response.writeHead(413).end(); return; } chunks.push(chunk); }
      try {
        const bytes = Buffer.concat(chunks);
        const reports = JSON.parse(bytes);
        const delivered = await fetch(nel, { method: 'POST', headers: { 'Content-Type': 'application/reports+json' }, body: bytes });
        automatic.push({ status: delivered.status, count: reports.filter(report => report.type === 'network-error').length,
          stored: Number(delivered.headers.get('x-temps-sentry-logs-stored') || 0) });
        response.writeHead(delivered.status).end();
      } catch { response.writeHead(503).end(); }
      return;
    }
    if (request.url.startsWith('/fail')) { response.writeHead(503).end('Synthetic unavailable response'); return; }
    const reportingUrl = `https://localhost:${secure.address().port}/report`;
    response.writeHead(200, { 'Content-Type': 'text/html', 'Report-To': JSON.stringify({ group: 'synthetic-nel', max_age: 3600, endpoints: [{ url: reportingUrl }] }),
      'NEL': JSON.stringify({ report_to: 'synthetic-nel', max_age: 3600, success_fraction: 0, failure_fraction: 1 }) });
    response.end('<!doctype html><title>Synthetic network policy</title>');
  });
  await new Promise(resolve => secure.listen(0, '127.0.0.1', resolve));
  process.stderr.write('SYNTHETIC_STAGE=browser_launch\n');
  // Opt into the full browser's headless mode. The default headless shell
  // accepts these headers but produced no automatic NEL reports in the same
  // test; this is a browser test configuration issue, not a TLS/backend gap.
  browser = await chromium.launch({ channel: 'chromium', headless: true,
    args: ['--short-reporting-delay', '--ignore-certificate-errors-spki-list=' + spki] });
  assert.equal(await browser.version(), '156.0.8078.4');
  const context = await browser.newContext();
  const page = await context.newPage();
  process.stderr.write('SYNTHETIC_STAGE=csp_navigation\n');
  await page.goto(sourceUrl);
  assert.equal(await page.evaluate(() => window.syntheticBlocked), undefined, 'Browser CSP did not block the inline script');
  process.stderr.write('SYNTHETIC_STAGE=nel_navigation\n');
  await page.goto(`https://localhost:${secure.address().port}/${run}`);
  const failedUrl = `https://localhost:${secure.address().port}/fail?synthetic=${run}`;
  assert.equal(await page.evaluate(url => fetch(url).then(response => response.status), failedUrl), 503);
  const deadline = Date.now() + 15000;
  while (!automatic.some(result => result.count > 0) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 250));
  assert.ok(automatic.some(result => result.count > 0), 'Chromium did not automatically deliver NEL before the deadline');
  assert.ok(automatic.every(result => result.status === 200 && result.stored === result.count),
    'Browser-generated NEL did not persist through the native ingestion endpoint');
  // Keep the explicit DNS fixture as an additional schema/fidelity scenario;
  // it cannot substitute for the automatic browser reporting assertion above.
  const fixture = { age: 0, type: 'network-error', url: 'https://synthetic.example/' + run,
    body: { type: 'dns.name_not_resolved', phase: 'dns', elapsed_time: 12, method: 'GET', status_code: 0, sampling_fraction: 1, protocol: 'http/2', server_ip: '192.0.2.1' } };
  process.stderr.write('SYNTHETIC_STAGE=nel_fixture\n');
  const delivered = await fetch(nel, { method: 'POST', headers: { 'Content-Type': 'application/reports+json' }, body: JSON.stringify([fixture]) });
  process.stderr.write('SYNTHETIC_HTTP_STATUS=' + delivered.status + '\n');
  assert.equal(delivered.status, 200);
  assert.equal(delivered.headers.get('x-temps-sentry-logs-stored'), '1');
  assert.equal(delivered.headers.get('x-temps-sentry-unsupported-items'), '0');
  console.log(JSON.stringify({ browser: await browser.version(), browser_csp: true, source_url: sourceUrl,
    nel_fixture: true, automatic_nel: true, automatic_source_url: failedUrl,
    automatic_reports: automatic.reduce((n, result) => n + result.count, 0), run }));
} catch (error) {
  const detail = String(error.message).split('\n')[0].replace(/https?:\/\/\S+/g, '[synthetic-url]').slice(0, 200);
  process.stderr.write('SYNTHETIC_FAILURE=' + JSON.stringify({ name: error.name, detail }) + '\n');
  throw error;
} finally {
  if (browser) await browser.close();
  for (const server of [plain, secure]) if (server) { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
  await rm(directory, { recursive: true });
}

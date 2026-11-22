// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// CLI 3.8.0 + Node SDK 11.8.0, synthetic minified Node application. esbuild 0.25.10 builds the fixture.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
const [entry, cli, run, esbuildEntry] = process.argv.slice(2);
const S = await import(pathToFileURL(entry).href);
assert.equal(S.SDK_VERSION, '11.8.0');
const directory = await mkdtemp(join(tmpdir(), 'temps-synthetic-debug-id-'));
try {
  process.stderr.write('SYNTHETIC_STAGE=build\n');
  const source = join(directory, 'synthetic-original.ts');
  const dist = join(directory, 'dist');
  await writeFile(source, `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nexport function syntheticFailure() {\n const message = 'Synthetic Debug ID failure';\n throw new Error(message);\n}\n`);
  await mkdir(dist);
  const esbuild = await import(pathToFileURL(esbuildEntry).href);
  assert.equal(esbuild.version, '0.25.10');
  await esbuild.build({ entryPoints: [source], outfile: join(dist, 'application.js'), platform: 'node', format: 'cjs', minify: true, sourcemap: 'external', sourcesContent: true });
  const invoke = args => {
    const result = spawnSync(cli, args, { env: { ...process.env, SENTRY_RELEASE: '', SENTRY_LOG_LEVEL: 'error' }, encoding: 'utf8', timeout: 60000 });
    assert.equal(result.status, 0, 'Official sentry-cli failed; output suppressed');
  };
  process.stderr.write('SYNTHETIC_STAGE=inject\n');
  invoke(['sourcemaps', 'inject', dist]);
  const map = JSON.parse(await readFile(join(dist, 'application.js.map'), 'utf8'));
  assert.ok(map.debug_id || map.debugId, 'CLI did not inject a Debug ID');
  const debugId = map.debug_id || map.debugId;
  const receipts = [];
  let captured;
  const injected = await readFile(join(dist, 'application.js'), 'utf8');
  assert.ok(injected.includes('_sentryDebugIds') || injected.includes('_debugIds'), 'CLI emitted no runtime Debug ID registration');
  process.stderr.write('SYNTHETIC_STAGE=sdk_capture\n');
  S.init({ dsn: process.env.SENTRY_DSN, defaultIntegrations: false, sendClientReports: false,
    beforeSend(event) { captured = event; return event; },
    transport(options) {
      const transport = S.makeNodeTransport(options);
      return { send(envelope) { return transport.send(envelope).then(r => { receipts.push(r.statusCode); return r; }); }, flush: timeout => transport.flush(timeout) };
    },
  });
  S.setTag('synthetic.run', run);
  const require = createRequire(import.meta.url);
  let eventId;
  const application = require(join(dist, 'application.js'));
  assert.equal(typeof application.syntheticFailure, 'function', 'Synthetic fixture export missing');
  try { application.syntheticFailure(); }
  catch (error) { assert.equal(error.message, 'Synthetic Debug ID failure'); eventId = S.captureException(error); }
  process.stderr.write('SYNTHETIC_STAGE=sdk_flush\n');
  assert.ok(await S.flush(10000));
  process.stderr.write('SYNTHETIC_STAGE=sdk_debug_meta\n');
  const diagnostics = { frames: captured?.exception?.values?.flatMap(e => e.stacktrace?.frames ?? []).length ?? 0, injected_ids: Object.values(globalThis._sentryDebugIds ?? {}).length + Object.values(globalThis._debugIds ?? {}).length, expected_injected: [...Object.values(globalThis._sentryDebugIds ?? {}), ...Object.values(globalThis._debugIds ?? {})].includes(debugId), images: captured?.debug_meta?.images?.length ?? 0 };
  process.stderr.write('SYNTHETIC_DIAGNOSTIC=' + JSON.stringify(diagnostics) + '\n');
  assert.ok(captured.debug_meta?.images?.some(image => image.debug_id === debugId), 'Official SDK did not emit the injected Debug ID');
  process.stderr.write('SYNTHETIC_STAGE=sdk_release\n');
  assert.ok(!captured.release, 'This scenario must work without a release');
  // Import after the event to exercise read-time symbolication of late maps.
  process.stderr.write('SYNTHETIC_STAGE=upload\n');
  invoke(['sourcemaps', 'upload', '--org', 'temps', '--project', process.env.SYNTHETIC_PROJECT_SLUG, '--wait', dist]);
  invoke(['sourcemaps', 'upload', '--org', 'temps', '--project', process.env.SYNTHETIC_PROJECT_SLUG, '--wait', dist]);
  assert.ok(await S.close(10000));
  assert.ok(receipts.length && receipts.every(status => status === 200));
  console.log(JSON.stringify({ sdk: S.SDK_VERSION, cli: '3.8.0', debug_id: debugId, event_id: eventId, late_upload: true, repeated_upload: true, release: false }));
} finally { await rm(directory, { recursive: true }); }

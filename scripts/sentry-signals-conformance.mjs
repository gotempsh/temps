// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Synthetic official SDK traffic, pinned to sentry-javascript 10.17.0
// (eb6e2946eb45583dd4c02ec92f8912e5bad51943) and 11.8.0
// (c6492ef78c7bfb97931e43075763ce84ee8fda7e). No customer fixtures.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const [entry, dsn, mode = "traces", tunnel, appOrigin] = process.argv.slice(2);
if (!mode.startsWith("replay") && typeof Bun !== "undefined") {
  const child = spawnSync("node", [process.argv[1], ...process.argv.slice(2)], {
    stdio: "inherit",
  });
  if (child.error) throw child.error;
  process.exit(child.status ?? 1);
}
if (mode.startsWith("replay")) {
  const require = createRequire(pathToFileURL(entry));
  const { chromium } = await import(
    pathToFileURL(require.resolve("playwright")).href
  );
  const directory = await mkdtemp(join(tmpdir(), "temps-sentry-replay-"));
  const browser = await chromium.launch({ headless: true });
  try {
    const wrapper = join(directory, "entry.mjs");
    await writeFile(
      wrapper,
      `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nimport * as S from ${JSON.stringify(entry)}; window.SyntheticSentry=S;`,
    );
    const build = await Bun.build({
      entrypoints: [wrapper],
      target: "browser",
      format: "iife",
    });
    assert.equal(build.success, true);
    const page = await browser.newPage();
    const pageUrl = new URL(appOrigin ?? tunnel).origin + "/synthetic-replay";
    if (!appOrigin)
      await page.route(pageUrl, (route) =>
        route.fulfill({
          contentType: "text/html",
          body: '<!doctype html><title>Synthetic replay</title><button id="synthetic">Synthetic action</button><div id="result"></div>',
        }),
      );
    await page.goto(pageUrl);
    await page.addScriptTag({ content: await build.outputs[0].text() });
    const result = await page.evaluate(
      async ({ dsn, tunnel, mode }) => {
        const S = window.SyntheticSentry;
        const deliveries = [];
        const recordings = [];
        const signals = { spans: 0, metrics: 0 };
        let traceId;
        let native;
        S.init({
          dsn,
          tunnel: mode === "replay-tunnel" ? tunnel : undefined,
          defaultIntegrations: false,
          sendClientReports: false,
          tracesSampleRate: 1,
          dataCollection: { userInfo: false },
          release: "synthetic-release-1",
          environment: "synthetic-test",
          replaysSessionSampleRate: 1,
          replaysOnErrorSampleRate: 0,
          integrations: [
            S.replayIntegration({
              useCompression: mode !== "replay-uncompressed",
              maskAllText: true,
              maskAllInputs: true,
              minReplayDuration: 0,
              flushMinDelay: 0,
              flushMaxDelay: 1000,
            }),
          ],
          transport(options) {
            native = S.makeFetchTransport(options);
            return {
              send(e) {
                for (const [header, payload] of e[1]) {
                  if (header.type === "span")
                    signals.spans += payload.items.length;
                  if (header.type === "transaction")
                    signals.spans += 1 + (payload.spans?.length ?? 0);
                  if (header.type === "trace_metric")
                    signals.metrics += payload.items.length;
                }
                if (e[1].some(([h]) => h.type === "replay_recording")) {
                  recordings.push(e);
                  return Promise.resolve({ statusCode: 200 });
                }
                return native.send(e).then((r) => {
                  deliveries.push(r.statusCode);
                  return r;
                });
              },
              flush: (t) => native.flush(t),
            };
          },
        });
        const replay = S.getReplay();
        await new Promise((r) => setTimeout(r, 300));
        await S.startSpan(
          { name: "Synthetic browser root", op: "ui.action" },
          async (root) => {
            traceId = root.spanContext().traceId;
            await S.startSpan(
              { name: "Synthetic browser child", op: "ui.action" },
              async () => {
                S.metrics.count("synthetic.browser.requests", 1);
                S.metrics.gauge("synthetic.browser.queue", 7);
                S.metrics.distribution("synthetic.browser.duration", 12.5, {
                  unit: "millisecond",
                });
              },
            );
          },
        );
        document.querySelector("#result").textContent =
          "Synthetic first mutation";
        await new Promise((r) => setTimeout(r, 150));
        await replay.flush();
        document.querySelector("#result").textContent =
          "Synthetic second mutation";
        await new Promise((r) => setTimeout(r, 150));
        await replay.flush();
        const replayId = replay.getReplayId();
        await replay.stop();
        await S.flush(10000);
        // Race two first deliveries of one segment to exercise initialization
        // and native deduplication, then deliver remaining segments backwards.
        const reversed = [...recordings].reverse();
        const first = reversed.shift();
        if (first) {
          const concurrent = await Promise.all([
            native.send(first),
            native.send(first),
          ]);
          deliveries.push(...concurrent.map((r) => r.statusCode));
        }
        for (const e of reversed) {
          const r = await native.send(e);
          deliveries.push(r.statusCode);
        }
        for (const e of [...recordings].reverse()) {
          const r = await native.send(e);
          deliveries.push(r.statusCode);
        }
        await native.flush(10000);
        await S.close(10000);
        return {
          sdk: S.SDK_VERSION,
          mode,
          replay_id: replayId,
          trace_id: traceId,
          ...signals,
          segments: recordings.length,
          segment_ids: recordings.map(
            (e) => e[1].find(([h]) => h.type === "replay_event")[1].segment_id,
          ),
          binary: recordings.some(
            (e) =>
              e[1].find(([h]) => h.type === "replay_recording")[1] instanceof
              Uint8Array,
          ),
          deliveries,
        };
      },
      { dsn, tunnel, mode },
    );
    assert.equal(result.sdk, "11.8.0");
    assert.ok(result.segments >= 2);
    assert.ok(result.deliveries.every((s) => s === 200));
    assert.ok(result.replay_id);
    assert.equal(result.spans, 2);
    assert.equal(result.metrics, 3);
    if (mode !== "replay-uncompressed") assert.equal(result.binary, true);
    process.stdout.write(JSON.stringify(result) + "\n");
  } finally {
    await browser.close();
    await rm(directory, { recursive: true });
  }
  process.exit(0);
}
const S = await import(pathToFileURL(entry).href);
assert.ok(["10.17.0", "11.8.0"].includes(S.SDK_VERSION));
const deliveries = [];
const spans = [];
const transactions = [];
const metrics = [];
const logs = [];
const errors = [];
S.init({
  dsn,
  tunnel: mode.endsWith("-tunnel") ? tunnel : undefined,
  defaultIntegrations: false,
  sendClientReports: false,
  tracesSampleRate: 1,
  enableLogs: true,
  release: "synthetic-release-1",
  environment: "synthetic-test",
  beforeSendMetric(m) {
    m.attributes["server.address"] = "synthetic.example";
    return m;
  },
  beforeSend(e) {
    e.server_name = "synthetic.example";
    return e;
  },
  transport(options) {
    const native = S.makeNodeTransport(options);
    return {
      send(e) {
        for (const [h, p] of e[1]) {
          if (h.type === "span") spans.push(...p.items);
          if (h.type === "transaction") transactions.push(p);
          if (h.type === "trace_metric") metrics.push(...p.items);
          if (h.type === "log") logs.push(...p.items);
          if (h.type === "event") errors.push(p.event_id);
        }
        return native.send(e).then((r) => {
          deliveries.push(r.statusCode);
          return r;
        });
      },
      flush: (t) => native.flush(t),
    };
  },
});
let traceId;
await S.startSpan(
  {
    name: "Synthetic root",
    op: "http.server",
    attributes: { "service.name": "synthetic-service" },
  },
  async (root) => {
    traceId = root.spanContext().traceId;
    await S.startSpan(
      {
        name: "Synthetic child",
        op: "db.query",
        attributes: { "synthetic.flag": true },
      },
      async () => {
        if (S.metrics) {
          S.metrics.count("synthetic.requests", 3);
          S.metrics.count("synthetic.requests", 2);
          S.metrics.gauge("synthetic.queue", 7);
          S.metrics.distribution("synthetic.duration", 12.5, {
            unit: "millisecond",
          });
          S.metrics.distribution("synthetic.duration", 25, {
            unit: "millisecond",
          });
        }
        S.logger.info("Synthetic linked log", {
          "service.name": "synthetic-service",
        });
        errors.push(S.captureMessage("Synthetic linked message"));
      },
    );
  },
);
assert.equal(await S.flush(10000), true);
await S.close(10000);
assert.ok(deliveries.length > 0);
assert.ok(deliveries.every((s) => s === 200));
assert.ok(
  spans.length === 2 || transactions.some((t) => t.spans?.length === 1),
);
assert.equal(metrics.length, S.SDK_VERSION === "11.8.0" ? 5 : 0);
assert.equal(logs.length, 1);
process.stdout.write(
  JSON.stringify({
    sdk: S.SDK_VERSION,
    mode,
    trace_id: traceId,
    spans:
      spans.length ||
      transactions.reduce((n, t) => n + 1 + (t.spans?.length ?? 0), 0),
    metrics: metrics.length,
    logs: logs.length,
    deliveries,
  }) + "\n",
);

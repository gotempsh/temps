import { beforeAll, afterAll, test, expect } from "bun:test";
import { mkdtemp, rm, writeFile, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { startRunner } from "./runner.js";
import { createEvent } from "./model.js";
import { control, sessionPaths } from "./session.js";
let temp: string;
let binary: string;
const sessions: string[] = [];
const active: Awaited<ReturnType<typeof startRunner>>[] = [];
const session = () => {
  const s = `test-${crypto.randomUUID().slice(0, 12)}`;
  sessions.push(s);
  return s;
};
beforeAll(async () => {
  temp = await mkdtemp(join(tmpdir(), "temps-plugin-dev-test-"));
  binary = join(temp, "example");
  const build = Bun.spawn(
    [
      "bun",
      "build",
      "--compile",
      resolve("examples/plugin-dev.ts"),
      "--outfile",
      binary,
    ],
    { stdout: "ignore", stderr: "pipe" },
  );
  if (await build.exited)
    throw new Error(await new Response(build.stderr).text());
}, 60000);
afterAll(async () => {
  for (const r of active) await r.stop();
  for (const name of sessions)
    await rm(sessionPaths(name).state, { recursive: true, force: true });
  await rm(temp, { recursive: true, force: true });
});
async function state(r: Awaited<ReturnType<typeof startRunner>>) {
  for (let i = 0; i < 100; i++) {
    const response = await fetch(`${r.url}/x/deployment-journal/api/state`);
    if (response.ok) return response.json();
    await Bun.sleep(20);
  }
  throw new Error("Plugin never initialized");
}
async function waitEvents(
  r: Awaited<ReturnType<typeof startRunner>>,
  count: number,
) {
  for (let i = 0; i < 100; i++) {
    const data = await state(r);
    if (data.events.length === count) return data;
    await Bun.sleep(20);
  }
  throw new Error(`Expected ${count} persisted plugin events`);
}
test("compiled plugin: UI, discovery, events, duplicates, HTTP fallback, revocation and restart", async () => {
  const name = session();
  let r = await startRunner({
    session: name,
    command: [binary],
    grants: ["events_read", "ai_generate"],
    fixtures: { ai: { mode: "success" } },
  });
  active.push(r);
  const first = await state(r);
  expect(first.capabilities.permissions).toEqual([
    "events_read",
    "ai_generate",
  ]);
  const page = await fetch(r.url + "/");
  expect(await page.text()).toContain("Deployment journal");
  const event = createEvent("deployment.succeeded", {
    url: "https://example.com",
  });
  expect(await control(name, "emit", { event })).toMatchObject({
    status: "sent",
    handler_completed: false,
  });
  await waitEvents(r, 1);
  await control(name, "emit", { event });
  await Bun.sleep(30);
  expect((await state(r)).events).toHaveLength(1);
  expect(
    await control(name, "emit", {
      event: createEvent("deployment.failed", {}),
      transport: "http",
    }),
  ).toMatchObject({ status: "http_accepted" });
  await waitEvents(r, 2);
  await expect(
    control(name, "emit", { event: createEvent("project.created", {}) }),
  ).rejects.toThrow("subscribe");
  const cookie = (await fetch(r.url + "/", { redirect: "manual" })).headers
    .get("set-cookie")!
    .split(";")[0]!;
  const ai = await fetch(r.url + "/x/deployment-journal/api/ai", {
    method: "POST",
    headers: { origin: r.url, cookie },
  });
  expect(await ai.json()).toMatchObject({ model: "temps-local-mock" });
  await control(name, "grants", { grants: [] });
  await expect(control(name, "emit", { event })).rejects.toThrow("events_read");
  expect((await state(r)).capabilities.permissions).toEqual([]);
  const denied = await fetch(r.url + "/x/deployment-journal/api/ai", {
    method: "POST",
    headers: { origin: r.url, cookie },
  });
  expect(denied.status).toBe(400);
  await r.stop();
  r = await startRunner({ session: name, command: [binary] });
  active.push(r);
  const restored = await state(r);
  expect(restored.events).toHaveLength(2);
  expect(restored.capabilities.actor.id).toBe(first.capabilities.actor.id);
  expect(restored.capabilities.permissions).toEqual([]);
  await r.stop();
}, 30000);
test("source plugin and preview reject spoofing, cross-site writes and reserved routes", async () => {
  const r = await startRunner({
    session: session(),
    command: ["bun", resolve("examples/plugin-dev.ts")],
    grants: ["api_write"],
    role: "reader",
  });
  active.push(r);
  await state(r);
  expect((await state(r)).capabilities.permissions).toEqual([]); // undeclared grant is ineffective
  const spoof = await fetch(r.url + "/x/deployment-journal/api/state", {
    headers: {
      "x-temps-user-role": "admin",
      "x-temps-auth-signature": "attacker",
    },
  });
  expect((await spoof.json()).role).toBe("reader");
  expect(
    (await fetch(r.url + "/", { headers: { host: "attacker.example" } }))
      .status,
  ).toBe(403);
  expect(
    (
      await fetch(r.url + "/", {
        headers: { origin: "https://attacker.example" },
      })
    ).status,
  ).toBe(403);
  expect(
    (await fetch(r.url + "/x/deployment-journal/api/ai", { method: "POST" }))
      .status,
  ).toBe(403);
  for (const path of ["/_events", "/_temps/channel", "/%5fevents"])
    expect((await fetch(r.url + "/x/deployment-journal" + path)).status).toBe(
      403,
    );
  expect(
    (await fetch("http://localhost/api/state", { unix: r.paths.socket }))
      .status,
  ).toBe(401);
  expect(
    (await control(sessions.at(-1)!, "logs")).every(
      (e: any) => !JSON.stringify(e).includes("attacker"),
    ),
  ).toBe(true);
  await r.stop();
}, 15000);
test("startup failures release locks and do not hang", async () => {
  for (const command of [
    [join(temp, "missing")],
    ["bun", "-e", 'console.log("bad")'],
    ["bun", "-e", "setInterval(()=>{},1000)"],
  ]) {
    const name = session();
    await expect(
      startRunner({ session: name, command, startupTimeout: 200 }),
    ).rejects.toThrow();
    expect(await Bun.file(sessionPaths(name).control).exists()).toBe(false);
  }
}, 15000);
test("occupied sessions are rejected without touching the running plugin", async () => {
  const name = session();
  const r = await startRunner({ session: name, command: [binary] });
  active.push(r);
  await expect(
    startRunner({ session: name, command: [binary] }),
  ).rejects.toThrow("occupied");
  expect((await state(r)).capabilities.actor.active).toBe(true);
  await r.stop();
}, 15000);
test("CLI parsing routes emit/status/grants to the named session", async () => {
  const name = session();
  const r = await startRunner({
    session: name,
    command: [binary],
    grants: ["events_read"],
  });
  active.push(r);
  await state(r);
  for (const args of [
    ["status", "--json"],
    ["emit", "deployment.succeeded", "--repeat", "2", "--json"],
    ["grants", "set", "--clear"],
  ]) {
    const p = Bun.spawn(
      ["bun", "src/index.ts", "plugin", "dev", ...args, "--session", name],
      { stdout: "pipe", stderr: "pipe" },
    );
    const output = await new Response(p.stdout).text();
    const err = await new Response(p.stderr).text();
    expect(await p.exited, err).toBe(0);
    expect(() => JSON.parse(output)).not.toThrow();
  }
  expect((await state(r)).capabilities.permissions).toEqual([]);
  expect((await waitEvents(r, 1)).events).toHaveLength(1);
  await r.stop();
}, 30000);

test("proxy isolates browser cookies and strips plugin-set cookies", async () => {
  const source = join(temp, "cookie-plugin.ts");
  const sdk = resolve("../../sdks/node/packages/plugin-sdk/src/runtime.ts");
  await writeFile(
    source,
    `import {runPlugin} from ${JSON.stringify(sdk)}; await runPlugin({ manifest:()=>({name:'cookie-test',version:'1',nav:[],requires_db:false,health_path:'/health',events:[]}), handler:()=> (req,res)=>{res.writeHead(200,{'content-type':'application/json','set-cookie':'other_app=hijacked; Path=/'});res.end(JSON.stringify({cookie:req.headers.cookie??null}));} });`,
  );
  const r = await startRunner({ session: session(), command: ["bun", source] });
  active.push(r);
  let response: Response | undefined;
  for (let i = 0; i < 50; i++) {
    response = await fetch(r.url + "/x/cookie-test/", {
      headers: { cookie: "other_app=private" },
    });
    if (response.ok) break;
    await Bun.sleep(20);
  }
  expect(await response!.json()).toEqual({ cookie: null });
  expect(response!.headers.has("set-cookie")).toBe(false);
  await r.stop();
}, 15000);
test("shutdown kills descendants launched through a wrapper", async () => {
  const script = join(temp, "wrapper.sh");
  const pidFile = join(temp, "descendant.pid");
  await writeFile(
    script,
    `#!/bin/sh\nsleep 120 &\necho $! > '${pidFile}'\nexec '${binary}' "$@"\n`,
    { mode: 0o700 },
  );
  const r = await startRunner({ session: session(), command: [script] });
  active.push(r);
  await state(r);
  const pid = Number(await readFile(pidFile, "utf8"));
  expect(pid).toBeGreaterThan(1);
  process.kill(pid, 0);
  await r.stop();
  let alive = true;
  for (let i = 0; i < 50; i++) {
    try {
      process.kill(pid, 0);
    } catch {
      alive = false;
      break;
    }
    await Bun.sleep(20);
  }
  expect(alive).toBe(false);
}, 15000);
test("interrupted startup cleans the child and session lock", async () => {
  const name = session();
  const abort = new AbortController();
  const starting = startRunner({
    session: name,
    command: ["bun", "-e", "setInterval(()=>{},1000)"],
    signal: abort.signal,
  });
  setTimeout(() => abort.abort(), 50);
  await expect(starting).rejects.toThrow("interrupted");
  const r = await startRunner({ session: name, command: [binary] });
  active.push(r);
  await r.stop();
}, 15000);

// Build first with: cargo build -p temps-plugin-sdk --example plugin-dev-probe
// CLI-only contributors need no Rust toolchain. The release verification runs both.
const rustProbe = resolve("../../target/debug/examples/plugin-dev-probe");
test.skipIf(!existsSync(rustProbe))(
  "Rust SDK decodes host responses and receives authenticated events",
  async () => {
    const name = session();
    const r = await startRunner({
      session: name,
      command: [rustProbe],
      grants: ["events_read", "projects_read"],
    });
    active.push(r);
    const capabilities = await fetch(
      r.url + "/api/x/rust-dev-probe/capabilities",
    );
    expect(capabilities.status).toBe(200);
    expect(await capabilities.json()).toMatchObject({
      permissions: ["events_read", "projects_read"],
    });
    const project = await fetch(r.url + "/api/x/rust-dev-probe/project");
    expect(project.status).toBe(200);
    expect(await project.json()).toMatchObject({ id: 1, name: "demo" });
    const event = createEvent("deployment.succeeded", {});
    await control(name, "emit", { event });
    await control(name, "emit", { event, transport: "http" });
    let events: unknown[] = [];
    for (let i = 0; i < 50; i++) {
      events = await (
        await fetch(r.url + "/api/x/rust-dev-probe/events")
      ).json();
      if (events.length) break;
      await Bun.sleep(20);
    }
    expect(events).toHaveLength(1);
    await control(name, "grants", { grants: [] });
    expect((await fetch(r.url + "/api/x/rust-dev-probe/project")).status).toBe(
      502,
    );
    await r.stop();
  },
  15000,
);

test("CLI --exec preserves an argument vector and shuts down on SIGTERM", async () => {
  const name = session();
  const p = Bun.spawn(
    [
      "bun",
      "src/index.ts",
      "plugin",
      "dev",
      "--session",
      name,
      "--exec",
      "bun",
      "--",
      "run",
      resolve("examples/plugin-dev.ts"),
    ],
    { stdout: "ignore", stderr: "pipe" },
  );
  const errors = new Response(p.stderr).text();
  try {
    let status: { plugin?: string } | undefined;
    for (let i = 0; i < 100; i++) {
      try {
        status = await control(name, "status");
        break;
      } catch {
        await Bun.sleep(30);
      }
    }
    expect(status?.plugin).toBe("deployment-journal");
  } finally {
    p.kill("SIGTERM");
    await p.exited;
    await errors;
  }
  expect(await Bun.file(sessionPaths(name).control).exists()).toBe(false);
}, 15000);

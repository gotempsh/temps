// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Run after `bun run build`: bun run scripts/test-plugin-dev-package.ts
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { sessionPaths } from "../src/commands/plugin/dev/session.js";
const root = resolve(import.meta.dir, "..");
const temp = await mkdtemp(join(tmpdir(), "temps-plugin-package-"));
const session = `package-${crypto.randomUUID().slice(0, 12)}`;
let runner: Bun.Subprocess<"ignore", "ignore", "pipe"> | undefined;
let stderr: Promise<string> | undefined;
async function run(args: string[], cwd = temp, success = true) {
  const p = Bun.spawn(args, { cwd, stdout: "pipe", stderr: "pipe" });
  const [out, error, code] = await Promise.all([
    new Response(p.stdout).text(),
    new Response(p.stderr).text(),
    p.exited,
  ]);
  if (success) assert.equal(code, 0, `${args.join(" ")} failed: ${error}`);
  return { out, error, code };
}
const cli = (...args: string[]) => [
  "bunx",
  "@temps-sdk/cli",
  "plugin",
  "dev",
  ...args,
];
try {
  await mkdir(join(temp, "pack"));
  await run(["bun", "pm", "pack", "--destination", join(temp, "pack")], root);
  const pkg = await Bun.file(join(root, "package.json")).json();
  const tarball = join(temp, "pack", `temps-sdk-cli-${pkg.version}.tgz`);
  await writeFile(
    join(temp, "package.json"),
    JSON.stringify({
      private: true,
      dependencies: { "@temps-sdk/cli": tarball },
    }),
  );
  await run(["bun", "install"]);
  const binary = join(temp, "plugin");
  await run([
    "bun",
    "build",
    "--compile",
    join(root, "examples/plugin-dev.ts"),
    "--outfile",
    binary,
  ]);
  runner = Bun.spawn(
    cli(
      binary,
      "--session",
      session,
      "--grant",
      "events_read",
      "ai_generate",
      "--fixtures",
      join(root, "examples/plugin-dev-fixtures.json"),
    ),
    { cwd: temp, stdin: "ignore", stdout: "ignore", stderr: "pipe" },
  );
  stderr = new Response(runner.stderr).text();
  let status: { preview: string } | undefined;
  for (let i = 0; i < 30; i++) {
    const result = await run(
      cli("status", "--session", session, "--json"),
      temp,
      false,
    );
    if (result.code === 0) {
      status = JSON.parse(result.out);
      break;
    }
    await Bun.sleep(100);
  }
  assert.ok(status, "Packaged CLI did not start");
  const url = new URL(status.preview).origin;
  const first = await fetch(url + "/", { redirect: "manual" });
  const cookie = first.headers.get("set-cookie")!.split(";")[0]!;
  assert.match(await (await fetch(url + "/")).text(), /Deployment journal/);
  const emitted = JSON.parse(
    (
      await run(
        cli(
          "emit",
          "deployment.succeeded",
          "--session",
          session,
          "--repeat",
          "2",
          "--url",
          "https://example.com",
          "--json",
        ),
      )
    ).out,
  );
  assert.equal(emitted.deliveries.length, 2);
  assert.equal(emitted.deliveries[0].id, emitted.deliveries[1].id);
  let state: any;
  for (let i = 0; i < 50; i++) {
    state = await (
      await fetch(url + "/api/x/deployment-journal/api/state")
    ).json();
    if (state.events.length === 1) break;
    await Bun.sleep(20);
  }
  assert.equal(state.events.length, 1, "Plugin must persist one unique event");
  const ai = await fetch(url + "/api/x/deployment-journal/api/ai", {
    method: "POST",
    headers: { cookie, origin: url },
  });
  assert.equal((await ai.json()).model, "temps-local-mock");
  await run(cli("grants", "set", "--session", session, "--clear"));
  const denied = await run(
    cli("emit", "deployment.succeeded", "--session", session),
    temp,
    false,
  );
  assert.notEqual(denied.code, 0);
  assert.match(denied.error, /events_read/);
  runner.kill("SIGTERM");
  await Promise.race([
    runner.exited,
    Bun.sleep(5000).then(() => {
      throw new Error("Packaged CLI did not stop after SIGTERM");
    }),
  ]);
  assert.equal(
    await Bun.file(sessionPaths(session).control).exists(),
    false,
    "Control socket must be removed",
  );
  console.log(
    "PASS: packed bunx CLI → native SDK plugin → UI, duplicate events, mock AI, revocation, and clean shutdown",
  );
} finally {
  if (runner && runner.exitCode === null) runner.kill("SIGTERM");
  if (runner) await runner.exited;
  await stderr;
  await rm(sessionPaths(session).state, { recursive: true, force: true });
  await rm(temp, { recursive: true, force: true });
}

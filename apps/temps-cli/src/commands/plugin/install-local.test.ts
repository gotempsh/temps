// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("install-local requires consent and uploads the approved bytes without a client path", async () => {
  const dir = await mkdtemp(join(tmpdir(), "plugin-upload-"));
  const path = join(dir, "private-fixture");
  const bytes = "fixture bytes";
  const digest = createHash("sha256").update(bytes).digest("hex");
  const uploads: {
    query: string;
    digest: string | null;
    bytes?: string;
    filename?: string;
  }[] = [];
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(req) {
      const url = new URL(req.url);
      if (url.pathname !== "/api/x/plugins/install/file")
        return new Response(null, { status: 404 });
      const file = (await req.formData()).get("file");
      uploads.push({
        query: url.search,
        digest: req.headers.get("X-Temps-Trust-Sha256"),
        bytes: file instanceof File ? await file.text() : undefined,
        filename: file instanceof File ? file.name : undefined,
      });
      return Response.json({
        name: "fixture",
        version: "1",
        sha256: digest,
        message: "Installed",
        platform: "test",
      });
    },
  });
  async function run(...args: string[]) {
    const proc = Bun.spawn(
      [
        process.execPath,
        "run",
        new URL("../../index.ts", import.meta.url).pathname,
        "plugin",
        "install-local",
        path,
        "--name",
        "fixture",
        "--plugin-version",
        "1",
        ...args,
      ],
      {
        env: {
          PATH: process.env.PATH,
          XDG_CONFIG_HOME: dir,
          TEMPS_API_URL: String(server.url),
          TEMPS_API_KEY: "tk_test",
          TEMPS_TOKEN: "tk_test",
        },
        stdin: "ignore",
        stdout: "pipe",
        stderr: "pipe",
      },
    );
    return {
      code: await proc.exited,
      stderr: await new Response(proc.stderr).text(),
    };
  }
  try {
    await writeFile(path, bytes);
    const unapproved = await run();
    expect(unapproved.code).not.toBe(0);
    expect(unapproved.stderr).toContain("--yes");
    expect(uploads).toHaveLength(0);
    const approved = await run("--yes");
    expect(approved.code, approved.stderr).toBe(0);
    expect(uploads).toHaveLength(1);
    expect(uploads[0]?.digest).toBe(digest);
    expect(uploads[0]?.bytes).toBe(bytes);
    expect(uploads[0]?.query).toBe("?name=fixture&version=1");
    expect(uploads[0]?.filename).not.toContain(dir);
  } finally {
    server.stop(true);
    await rm(dir, { recursive: true, force: true });
  }
}, 30000);

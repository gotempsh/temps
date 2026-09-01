// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createEmbeddedUiHandler, createUiHandler } from "./ui.js";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function responseRecorder() {
  let status = 0;
  let body = "";

  return {
    response: {
      writeHead(nextStatus: number) {
        status = nextStatus;
        return this;
      },
      end(chunk?: Buffer | string) {
        body = chunk?.toString() ?? "";
        return this;
      },
    },
    result: () => ({ status, body }),
  };
}

describe("createUiHandler", () => {
  it("serves files contained by the configured UI directory", () => {
    const directory = mkdtempSync(join(tmpdir(), "temps-plugin-ui-"));
    temporaryDirectories.push(directory);
    writeFileSync(join(directory, "index.html"), "plugin UI");

    const recorder = responseRecorder();
    const handled = createUiHandler(directory)(
      { url: "/ui/index.html" } as never,
      recorder.response as never,
    );

    expect(handled).toBe(true);
    expect(recorder.result()).toEqual({ status: 200, body: "plugin UI" });
  });

  it.each(["/ui/../secret.txt", "/ui/%2e%2e/secret.txt", "/ui//etc/passwd"])(
    "rejects traversal path %s",
    (url) => {
      const directory = mkdtempSync(join(tmpdir(), "temps-plugin-ui-"));
      temporaryDirectories.push(directory);
      writeFileSync(join(directory, "index.html"), "plugin UI");

      const recorder = responseRecorder();
      const handled = createUiHandler(directory)(
        { url } as never,
        recorder.response as never,
      );

      expect(handled).toBe(true);
      expect(recorder.result().status).toBe(403);
    },
  );

  it("returns a bad request for invalid URL encoding", () => {
    const directory = mkdtempSync(join(tmpdir(), "temps-plugin-ui-"));
    temporaryDirectories.push(directory);

    const recorder = responseRecorder();
    createUiHandler(directory)(
      { url: "/ui/%E0%A4%A" } as never,
      recorder.response as never,
    );

    expect(recorder.result().status).toBe(400);
  });
});

describe("createEmbeddedUiHandler", () => {
  it.each(["/ui/../secret.txt", "/ui/%2e%2e/secret.txt", "/ui//secret.txt"])(
    "rejects traversal path %s",
    (url) => {
      const recorder = responseRecorder();
      const handler = createEmbeddedUiHandler(
        new Map([
          [
            "index.html",
            {
              content: Buffer.from("plugin UI"),
              contentType: "text/html",
              immutable: false,
            },
          ],
        ]),
      );

      expect(handler({ url } as never, recorder.response as never)).toBe(true);
      expect(recorder.result().status).toBe(403);
    },
  );
});

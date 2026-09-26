// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, spyOn } from "bun:test";
import { errorFields, log } from "./log.js";

describe("log", () => {
  it("writes one JSON object per line with level, component and fields", () => {
    const out = spyOn(console, "log").mockImplementation(() => {});
    log("info", "migrate", "applied", { file: "004.sql" });
    const line = JSON.parse(out.mock.calls[0]![0] as string);
    out.mockRestore();

    expect(line).toMatchObject({ level: "info", component: "migrate", msg: "applied", file: "004.sql" });
    expect(new Date(line.ts).toISOString()).toBe(line.ts);
  });

  it("sends warnings and errors to stderr with the error's details", () => {
    const err = spyOn(console, "error").mockImplementation(() => {});
    log("error", "events", "db insert failed", errorFields(new Error("boom")));
    const line = JSON.parse(err.mock.calls[0]![0] as string);
    err.mockRestore();

    expect(line).toMatchObject({ level: "error", component: "events", error: "boom", error_name: "Error" });
    expect(typeof line.stack).toBe("string");
  });

  it("describes non-Error throwables", () => {
    expect(errorFields("nope")).toEqual({ error: "nope" });
  });
});

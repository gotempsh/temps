// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { countryForIp, initGeo } from "./geo.js";

// The real DB is gitignored (MaxMind license), so tests that need it only run
// where it has been provisioned locally.
const REAL_DB = join(import.meta.dir, "../data/GeoLite2-Country.mmdb");
const HAS_REAL_DB = existsSync(REAL_DB);

const tmp = mkdtempSync(join(tmpdir(), "geo-test-"));
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

describe("initGeo", () => {
  it("fails startup when required and the DB is missing, naming the path and the fix", async () => {
    const missing = join(tmp, "nope.mmdb");
    const err = await initGeo({ required: true, path: missing }).catch((e) => e);
    expect(err).toBeInstanceOf(Error);
    expect(err.message).toContain(missing);
    expect(err.message).toContain("telemetry-api/data/");
    expect(countryForIp("8.8.8.8")).toBeNull();
  });

  it("fails startup when required and the file is not a usable GeoLite2 DB", async () => {
    const corrupt = join(tmp, "corrupt.mmdb");
    writeFileSync(corrupt, "not a maxmind database".repeat(100));
    await expect(initGeo({ required: true, path: corrupt })).rejects.toThrow(corrupt);
  });

  it("degrades to null countries when not required", async () => {
    await initGeo({ required: false, path: join(tmp, "nope.mmdb") });
    expect(countryForIp("8.8.8.8")).toBeNull();
  });

  it.skipIf(!HAS_REAL_DB)("loads a real DB and resolves public IPs only", async () => {
    await initGeo({ required: true, path: REAL_DB });
    expect(countryForIp("8.8.8.8")).toBe("US");
    expect(countryForIp("10.0.0.1")).toBeNull();
  });
});

describe("verify-geo.ts (Dockerfile build step)", () => {
  const script = join(import.meta.dir, "verify-geo.ts");
  const run = (path: string) =>
    Bun.spawnSync(["bun", script], { env: { ...process.env, GEOLITE2_COUNTRY_DB: path } });

  it("fails the build with a structured error when the DB is missing", () => {
    const res = run(join(tmp, "nope.mmdb"));
    expect(res.exitCode).toBe(1);
    const line = JSON.parse(res.stderr.toString().trim().split("\n").pop()!);
    expect(line.level).toBe("error");
    expect(line.component).toBe("geo");
    expect(line.error).toContain("nope.mmdb");
  });

  it.skipIf(!HAS_REAL_DB)("passes with a real DB", () => {
    expect(run(REAL_DB).exitCode).toBe(0);
  });
});

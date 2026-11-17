// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect } from "bun:test";
import {
  conflictingLimits,
  ingestLimitsFromEnv,
  maxFailureReportBodyBytes,
  positiveIntFromEnv,
} from "./limits.js";

describe("positiveIntFromEnv", () => {
  it("uses the default when unset or blank", () => {
    expect(positiveIntFromEnv("X", 5, {})).toBe(5);
    expect(positiveIntFromEnv("X", 5, { X: "  " })).toBe(5);
  });

  it("reads a positive whole number", () => {
    expect(positiveIntFromEnv("X", 5, { X: "120" })).toBe(120);
    expect(positiveIntFromEnv("X", 5, { X: " 7 " })).toBe(7);
  });

  it("falls back to the default instead of removing the limit on a bad value", () => {
    for (const bad of ["0", "-1", "1.5", "abc", "10abc", "Infinity"]) {
      expect(positiveIntFromEnv("X", 5, { X: bad })).toBe(5);
    }
  });
});

describe("ingestLimitsFromEnv", () => {
  it("has defaults for every limit", () => {
    expect(ingestLimitsFromEnv({})).toEqual({
      maxRequestBodyBytes: 2 * 1024 * 1024,
      eventPropertiesMaxBytes: 8 * 1024,
      eventsBatchMaxEvents: 100,
      tempsVersionMaxChars: 64,
      failureReportMaxChars: 200_000,
      eventsPerMinute: 600,
      failureReportsPerMinute: 30,
    });
  });

  it("reads each limit from its variable", () => {
    expect(
      ingestLimitsFromEnv({
        MAX_REQUEST_BODY_BYTES: "1048576",
        EVENT_PROPERTIES_MAX_BYTES: "4096",
        EVENTS_BATCH_MAX_EVENTS: "50",
        TEMPS_VERSION_MAX_CHARS: "32",
        FAILURE_REPORT_MAX_CHARS: "100000",
        EVENTS_RATE_LIMIT_PER_MINUTE: "60",
        FAILURE_REPORT_RATE_LIMIT_PER_MINUTE: "10",
      })
    ).toEqual({
      maxRequestBodyBytes: 1048576,
      eventPropertiesMaxBytes: 4096,
      eventsBatchMaxEvents: 50,
      tempsVersionMaxChars: 32,
      failureReportMaxChars: 100000,
      eventsPerMinute: 60,
      failureReportsPerMinute: 10,
    });
  });

  it("fits the costliest failure report the defaults allow under the default body cap", () => {
    // 200,000 control characters: each JSON-escapes to 6 bytes.
    const body = JSON.stringify({
      report_text: "\u0001".repeat(200_000),
      failed_job_id: "build_image",
      failed_job_type: "BuildImageJob",
    });
    expect(Buffer.byteLength(body)).toBeLessThan(ingestLimitsFromEnv({}).maxRequestBodyBytes);
  });
});

describe("conflictingLimits", () => {
  it("finds nothing wrong with the defaults", () => {
    expect(conflictingLimits(ingestLimitsFromEnv({}))).toEqual([]);
  });

  it("flags a rate limit below the batch size", () => {
    const conflicts = conflictingLimits(
      ingestLimitsFromEnv({ EVENTS_RATE_LIMIT_PER_MINUTE: "50", EVENTS_BATCH_MAX_EVENTS: "100" })
    );
    expect(conflicts).toHaveLength(1);
    expect(conflicts[0]).toContain("EVENTS_RATE_LIMIT_PER_MINUTE (50)");
  });

  it("flags a body cap too small for the largest failure report", () => {
    const conflicts = conflictingLimits(
      ingestLimitsFromEnv({ MAX_REQUEST_BODY_BYTES: "100000", FAILURE_REPORT_MAX_CHARS: "200000" })
    );
    expect(conflicts).toHaveLength(1);
    expect(conflicts[0]).toContain("MAX_REQUEST_BODY_BYTES (100000)");
  });
});

describe("maxFailureReportBodyBytes", () => {
  const limits = ingestLimitsFromEnv({ FAILURE_REPORT_MAX_CHARS: "1000" });
  const bodyBytes = (text: string) =>
    Buffer.byteLength(
      JSON.stringify({
        report_text: text,
        temps_version: "0.1.36",
        project_id: 2147483647,
        deployment_id: 2147483647,
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      })
    );

  it("bounds the real JSON size of the largest report, whatever its characters", () => {
    const bound = maxFailureReportBodyBytes(limits);
    for (const unit of ["x", "\u0001", "\"", "\\", "€", "😀".slice(0, 1), "😀"]) {
      // `length` counts UTF-16 units, as the route's limit does.
      const text = unit.repeat(Math.floor(1000 / unit.length));
      expect(text.length).toBeLessThanOrEqual(1000);
      expect(bodyBytes(text)).toBeLessThanOrEqual(bound);
    }
  });
});

describe("conflictingLimits with multibyte reports", () => {
  it("flags a body cap that fits the characters but not their worst-case bytes", () => {
    const conflicts = conflictingLimits(
      ingestLimitsFromEnv({ MAX_REQUEST_BODY_BYTES: "250000", FAILURE_REPORT_MAX_CHARS: "200000" })
    );
    expect(conflicts).toHaveLength(1);
    expect(conflicts[0]).toContain("FAILURE_REPORT_MAX_CHARS (200000)");
  });

  it("accepts a body cap that fits the worst case", () => {
    const limits = ingestLimitsFromEnv({ FAILURE_REPORT_MAX_CHARS: "1000" });
    expect(
      conflictingLimits({ ...limits, maxRequestBodyBytes: maxFailureReportBodyBytes(limits) })
    ).toEqual([]);
  });
});

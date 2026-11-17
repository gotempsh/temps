// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Abuse limits for the public, unauthenticated ingest routes. Every self-hosted
// instance in the world can call them, so these are the only thing standing
// between a flood and an ever-growing database. Defaults are generous for real
// traffic (an instance sends a handful of small events a minute); each one can
// be tuned per deployment through the environment variable named beside it.

import { log } from "./log.js";

const KIB = 1024;
const MIB = 1024 * KIB;

// Reads a positive integer from the environment. Unset means the default; a
// value that is not a positive whole number is logged and also falls back to
// the default, so a typo can't silently remove a limit.
export function positiveIntFromEnv(
  name: string,
  fallback: number,
  env: Record<string, string | undefined> = process.env
): number {
  const raw = env[name];
  if (raw === undefined || raw.trim() === "") return fallback;
  const value = Number(raw.trim());
  if (!Number.isInteger(value) || value < 1) {
    log("warn", "config", "not a positive whole number; using the default", {
      variable: name,
      value: raw,
      default: fallback,
    });
    return fallback;
  }
  return value;
}

export interface IngestLimits {
  // Largest request body accepted on any route (Bun answers 413 above it).
  // Must fit the largest failure report once JSON-encoded (see
  // conflictingLimits for the worst case).
  maxRequestBodyBytes: number;
  // Largest `properties` object one event may carry, measured as UTF-8 JSON.
  eventPropertiesMaxBytes: number;
  // Most events accepted in one /v1/events/batch request.
  eventsBatchMaxEvents: number;
  // Longest temps_version string an event may carry.
  tempsVersionMaxChars: number;
  // Longest report_text a deploy-failure report may carry.
  failureReportMaxChars: number;
  // Events accepted per client IP per minute across /v1/events and
  // /v1/events/batch. A batch costs one per event, so batching can't multiply it.
  eventsPerMinute: number;
  // Requests accepted per client IP per minute on /v1/deploy-failure-reports.
  failureReportsPerMinute: number;
}

export function ingestLimitsFromEnv(
  env: Record<string, string | undefined> = process.env
): IngestLimits {
  return {
    maxRequestBodyBytes: positiveIntFromEnv("MAX_REQUEST_BODY_BYTES", 2 * MIB, env),
    eventPropertiesMaxBytes: positiveIntFromEnv("EVENT_PROPERTIES_MAX_BYTES", 8 * KIB, env),
    eventsBatchMaxEvents: positiveIntFromEnv("EVENTS_BATCH_MAX_EVENTS", 100, env),
    tempsVersionMaxChars: positiveIntFromEnv("TEMPS_VERSION_MAX_CHARS", 64, env),
    failureReportMaxChars: positiveIntFromEnv("FAILURE_REPORT_MAX_CHARS", 200_000, env),
    eventsPerMinute: positiveIntFromEnv("EVENTS_RATE_LIMIT_PER_MINUTE", 600, env),
    failureReportsPerMinute: positiveIntFromEnv("FAILURE_REPORT_RATE_LIMIT_PER_MINUTE", 30, env),
  };
}

// Bytes a failure report's JSON envelope adds around report_text (field names,
// ids, job names, version), with room to spare.
const FAILURE_REPORT_ENVELOPE_BYTES = 4 * KIB;

// The most bytes one character of report_text can take in the JSON body.
// report_text's length counts UTF-16 code units; the costliest unit is a
// control character or lone surrogate, which JSON escapes as `\uXXXX` (6
// bytes). Other units take fewer: a BMP character at most 3 bytes of UTF-8,
// and an astral character 4 bytes for its 2 units.
export const MAX_JSON_BYTES_PER_CHAR = 6;

// Bytes the largest failure report allowed by `failureReportMaxChars` can
// take, worst case.
export function maxFailureReportBodyBytes(limits: IngestLimits): number {
  return limits.failureReportMaxChars * MAX_JSON_BYTES_PER_CHAR + FAILURE_REPORT_ENVELOPE_BYTES;
}

// Combinations that are each valid but contradict one another, so a request
// the other limits allow could never succeed. Logged at startup; the values
// are still used as set, since the operator may intend them.
export function conflictingLimits(limits: IngestLimits): string[] {
  const conflicts: string[] = [];
  if (limits.eventsPerMinute < limits.eventsBatchMaxEvents) {
    conflicts.push(
      `EVENTS_RATE_LIMIT_PER_MINUTE (${limits.eventsPerMinute}) is below ` +
        `EVENTS_BATCH_MAX_EVENTS (${limits.eventsBatchMaxEvents}): a batch larger than ` +
        `the rate limit is always refused with 429`
    );
  }
  const reportBytes = maxFailureReportBodyBytes(limits);
  if (limits.maxRequestBodyBytes < reportBytes) {
    conflicts.push(
      `MAX_REQUEST_BODY_BYTES (${limits.maxRequestBodyBytes}) cannot fit a failure report of ` +
        `FAILURE_REPORT_MAX_CHARS (${limits.failureReportMaxChars}) characters, which can take ` +
        `up to ${reportBytes} bytes once JSON-encoded: such reports are refused with 413`
    );
  }
  return conflicts;
}

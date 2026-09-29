// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * CPU unit handling shared by the commands that read or write CPU settings.
 *
 * The API stores CPU in microcores (1_000_000 = one core). Users give the CLI
 * millicores (`environments resources --cpu 500`) or cores
 * (`projects update --cpu-limit 0.5`), so every write converts and every read
 * formats.
 */

export const MICROCORES_PER_CORE = 1_000_000
export const MICROCORES_PER_MILLICORE = 1000

/** Docker refuses a CPU limit below 0.01 cores; the API rejects it too. */
export const MIN_CPU_LIMIT_MILLICORES = 10

/**
 * Largest CPU value accepted, in millicores (256 cores). Well past any single
 * host, and low enough to catch microcore values: CLI 0.1.36 and earlier sent
 * `--cpu` unconverted, so the documented workaround was `--cpu 1000000` for one
 * core, which now means 1000 cores.
 */
export const MAX_CPU_MILLICORES = 256_000

export type CpuParseResult = { microcores: number } | { error: string }

/**
 * Parse a whole number of millicores (`1000` = one core) into microcores.
 * `label` names the flag in errors, e.g. "CPU" or "CPU request". The 0.01-core
 * floor only applies to limits: requests are recorded, not applied to Docker.
 */
export function parseMillicores(value: string, label: string, isLimit: boolean): CpuParseResult {
  const trimmed = value.trim()
  if (!/^\d+$/.test(trimmed) || Number(trimmed) <= 0) {
    return { error: `${label} must be a positive whole number of millicores (1000 = 1 core), got "${value}"` }
  }
  const millicores = Number(trimmed)
  if (isLimit && millicores < MIN_CPU_LIMIT_MILLICORES) {
    return { error: `${label} must be at least ${MIN_CPU_LIMIT_MILLICORES} millicores (0.01 cores), got ${millicores}` }
  }
  if (millicores > MAX_CPU_MILLICORES) {
    return {
      error:
        `${label} must be at most ${MAX_CPU_MILLICORES} millicores (256 cores), got ${millicores}. ` +
        `The value is in millicores (1000 = 1 core); if you were passing microcores for CLI 0.1.36 ` +
        `or earlier, divide by 1000.`,
    }
  }
  return { microcores: millicores * MICROCORES_PER_MILLICORE }
}

/** Parse a CPU limit in cores (`0.5`, `1`, `2`) into microcores. */
export function parseCores(value: string, label: string): CpuParseResult {
  const trimmed = value.trim()
  if (!/^(\d+(\.\d*)?|\.\d+)$/.test(trimmed) || Number(trimmed) <= 0) {
    return { error: `${label} must be a positive number of cores (e.g., 0.5, 1, 2), got "${value}"` }
  }
  const microcores = Math.round(Number(trimmed) * MICROCORES_PER_CORE)
  if (microcores < MIN_CPU_LIMIT_MILLICORES * MICROCORES_PER_MILLICORE) {
    return { error: `${label} must be at least 0.01 cores, got "${value}"` }
  }
  if (microcores > MAX_CPU_MILLICORES * MICROCORES_PER_MILLICORE) {
    return { error: `${label} must be at most 256 cores, got "${value}"` }
  }
  return { microcores }
}

/** Render a stored microcore value, e.g. `500000` → `500m (0.5 CPU)`. */
export function formatMicrocores(microcores: number): string {
  return `${microcores / MICROCORES_PER_MILLICORE}m (${microcores / MICROCORES_PER_CORE} CPU)`
}

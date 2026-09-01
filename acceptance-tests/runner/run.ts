#!/usr/bin/env bun
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Acceptance test orchestrator.
 *
 * Reads scenario YAMLs under `scenarios/`, runs each step deterministically,
 * dispatches `verify` blocks (deterministic + Claude judge), and writes a
 * markdown + JSON report.
 *
 * Failures don't abort — every scenario runs to completion so the report
 * surfaces all problems at once. Exit code is non-zero if any scenario
 * failed, so CI can gate on it.
 */

import { mkdir, readFile, readdir } from "node:fs/promises"
import { join, basename, dirname } from "node:path"
import { z } from "zod"
import { parse as parseYaml } from "yaml"

import { runStep, type RunContext, type StepResult } from "./tools.ts"
import { runVerify, type VerifyResult, type VerifyKind } from "./verifier.ts"
import { writeReport, type RunReport, type ScenarioReport, summarize } from "./report.ts"

// ---------------------------------------------------------------------------
// Scenario schema
//
// Loose by design: each step / verify carries its own kind-specific fields,
// validated downstream by the dispatcher. We only enforce the outer shape
// up front so a typo in a scenario file is caught before we run anything.
// ---------------------------------------------------------------------------

const StepSchema = z
  .object({
    label: z.string().optional(),
    action: z.string(),
  })
  .passthrough()

const VerifySchema = z
  .object({
    kind: z.string(),
  })
  .passthrough()

const ScenarioSchema = z.object({
  id: z.string(),
  title: z.string(),
  section: z.string(),
  docs: z.string().optional(),
  prerequisites: z.array(z.record(z.unknown())).optional(),
  initial_vars: z.record(z.unknown()).optional(),
  steps: z.array(StepSchema),
  verify: z.array(VerifySchema),
})

type Scenario = z.infer<typeof ScenarioSchema>

// ---------------------------------------------------------------------------
// CLI parsing — minimal; bun has built-in argv. No need for a flag parser dep.
// ---------------------------------------------------------------------------

interface Cli {
  baseUrl: string
  scenarioFilter?: string
  rootDir: string
}

function parseCli(): Cli {
  const argv = process.argv.slice(2)
  let baseUrl = process.env.TEMPS_BASE_URL ?? "http://localhost:3000"
  let scenarioFilter: string | undefined

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === "--against") {
      const next = argv[++i]
      if (!next) throw new Error("--against requires a URL")
      baseUrl = next
    } else if (arg === "--scenarios") {
      const next = argv[++i]
      if (!next) throw new Error("--scenarios requires a path or section name")
      scenarioFilter = next
    } else if (arg === "--help" || arg === "-h") {
      printHelp()
      process.exit(0)
    } else {
      throw new Error(`Unknown flag: ${arg}`)
    }
  }

  // Resolve the scenarios dir relative to this file so the runner works
  // regardless of cwd.
  const rootDir = join(import.meta.dir, "..")

  return { baseUrl, scenarioFilter, rootDir }
}

function printHelp(): void {
  console.log(`
acceptance-tests/runner

Usage:
  bun run run                                # all scenarios, default base URL
  bun run run --against URL                  # override target
  bun run run --scenarios SECTION            # one section (e.g. 03-storage)
  bun run run --scenarios PATH/TO/FILE.yaml  # one scenario

Required env:
  ANTHROPIC_API_KEY    needed for any scenario with claude_assert verifies
  DATABASE_URL         needed for any scenario with sql steps
  TEMPS_BASE_URL       (optional) default target if --against omitted
`)
}

// ---------------------------------------------------------------------------
// Scenario discovery
// ---------------------------------------------------------------------------

async function findScenarios(rootDir: string, filter?: string): Promise<string[]> {
  const scenariosDir = join(rootDir, "scenarios")
  const out: string[] = []

  if (filter && filter.endsWith(".yaml")) {
    // Direct path to a single scenario
    const direct = filter.startsWith("/") ? filter : join(scenariosDir, filter)
    out.push(direct)
    return out
  }

  // Walk one level: scenarios/<section>/*.yaml
  const sections = await readdir(scenariosDir, { withFileTypes: true })
  for (const section of sections) {
    if (!section.isDirectory()) continue
    if (filter && section.name !== filter && !filter.startsWith(section.name)) continue

    const sectionDir = join(scenariosDir, section.name)
    const files = await readdir(sectionDir)
    for (const f of files) {
      if (f.endsWith(".yaml") || f.endsWith(".yml")) {
        out.push(join(sectionDir, f))
      }
    }
  }

  out.sort()
  return out
}

// ---------------------------------------------------------------------------
// Scenario execution
// ---------------------------------------------------------------------------

async function runScenario(
  filePath: string,
  baseUrl: string,
  runDir: string,
): Promise<ScenarioReport> {
  const t0 = Date.now()
  const file = basename(filePath)
  const section = basename(dirname(filePath))

  let raw: string
  try {
    raw = await readFile(filePath, "utf8")
  } catch (e) {
    return {
      id: "?",
      title: `(failed to read ${file})`,
      section,
      file,
      ok: false,
      durationMs: Date.now() - t0,
      steps: [],
      verifies: [],
      firstFailure: `read error: ${(e as Error).message}`,
    }
  }

  let scenario: Scenario
  try {
    scenario = ScenarioSchema.parse(parseYaml(raw))
  } catch (e) {
    return {
      id: "?",
      title: `(invalid YAML: ${file})`,
      section,
      file,
      ok: false,
      durationMs: Date.now() - t0,
      steps: [],
      verifies: [],
      firstFailure: `parse error: ${(e as Error).message}`,
    }
  }

  const scenarioArtifactDir = join(runDir, "artifacts", scenario.id)
  await mkdir(scenarioArtifactDir, { recursive: true })

  const ctx: RunContext = {
    baseUrl,
    runDir,
    scenarioArtifactDir,
    vars: { ...(scenario.initial_vars ?? {}) },
  }

  const steps: ScenarioReport["steps"] = []
  const verifies: VerifyResult[] = []
  let lastResult: StepResult | null = null
  let firstFailure: string | undefined
  let aborted = false

  for (const [i, step] of scenario.steps.entries()) {
    const label = step.label ?? `step ${i + 1}`
    const action = step.action
    if (aborted) {
      steps.push({ label, action, ok: false, durationMs: 0, error: "skipped (earlier step failed)" })
      continue
    }
    try {
      const result = await runStep(
        // The schema is intentionally permissive; runStep narrows by `action`.
        step as unknown as Parameters<typeof runStep>[0],
        ctx,
      )
      lastResult = result
      steps.push({
        label,
        action,
        ok: result.ok,
        durationMs: result.durationMs,
        error: result.ok ? undefined : result.stderr || result.stdout,
      })
      if (!result.ok) {
        aborted = true
        firstFailure ??= `step '${label}' failed: ${result.stderr || result.stdout || "non-zero exit"}`
      }
    } catch (e) {
      const err = (e as Error).message
      steps.push({ label, action, ok: false, durationMs: 0, error: err })
      aborted = true
      firstFailure ??= `step '${label}' threw: ${err}`
    }
  }

  // Verifies always run, even if a step failed — so the report surfaces
  // every problem instead of stopping at the first.
  for (const spec of scenario.verify) {
    const last = lastResult ?? { ok: false, durationMs: 0 }
    try {
      const r = await runVerify(
        spec as unknown as { kind: VerifyKind } & Record<string, unknown>,
        last,
        ctx,
      )
      verifies.push(r)
      if (!r.ok) {
        firstFailure ??= `verify '${r.kind}' failed: ${r.reason}`
      }
    } catch (e) {
      const err = (e as Error).message
      verifies.push({
        ok: false,
        reason: `verify threw: ${err}`,
        kind: spec.kind as VerifyKind,
        durationMs: 0,
      })
      firstFailure ??= `verify '${spec.kind}' threw: ${err}`
    }
  }

  const ok = !aborted && verifies.every((v) => v.ok)

  return {
    id: scenario.id,
    title: scenario.title,
    section: scenario.section,
    file,
    ok,
    durationMs: Date.now() - t0,
    steps,
    verifies,
    firstFailure,
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main(): Promise<void> {
  const cli = parseCli()
  const runDir = join(cli.rootDir, "results", new Date().toISOString().replace(/[:.]/g, "-"))
  await mkdir(runDir, { recursive: true })

  const scenarioFiles = await findScenarios(cli.rootDir, cli.scenarioFilter)
  if (scenarioFiles.length === 0) {
    console.error(
      `No scenarios matched. Filter: ${cli.scenarioFilter ?? "(none)"}, root: ${cli.rootDir}`,
    )
    process.exit(2)
  }

  console.log(`acceptance-tests: ${scenarioFiles.length} scenario(s) against ${cli.baseUrl}`)
  console.log(`results: ${runDir}\n`)

  const startedAt = new Date().toISOString()
  const t0 = Date.now()
  const scenarios: ScenarioReport[] = []

  for (const file of scenarioFiles) {
    const rel = file.replace(cli.rootDir + "/", "")
    process.stdout.write(`  → ${rel} ... `)
    const report = await runScenario(file, cli.baseUrl, runDir)
    scenarios.push(report)
    console.log(report.ok ? `PASS (${(report.durationMs / 1000).toFixed(1)}s)` : `FAIL — ${report.firstFailure ?? ""}`)
  }

  const finishedAt = new Date().toISOString()
  const finalReport: RunReport = {
    baseUrl: cli.baseUrl,
    startedAt,
    finishedAt,
    durationMs: Date.now() - t0,
    scenarios,
  }
  writeReport(runDir, finalReport)

  const { passed, failed, total } = summarize(finalReport)
  console.log(`\n${passed}/${total} passed${failed > 0 ? ` · ${failed} FAILED` : ""}`)
  console.log(`report: ${join(runDir, "report.md")}`)
  process.exit(failed === 0 ? 0 : 1)
}

await main()

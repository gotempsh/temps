// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Report writer.
 *
 * After every run, dump a markdown report at `<runDir>/report.md` plus a
 * structured `results.json` next to it. Markdown is for humans (CI Slack
 * post, manual review); JSON is for trend tracking and "did this scenario
 * regress between releases" queries.
 */

import { writeFileSync } from "node:fs"
import { join } from "node:path"

import type { VerifyResult } from "./verifier.ts"

export interface ScenarioReport {
  id: string
  title: string
  section: string
  file: string
  ok: boolean
  durationMs: number
  steps: Array<{
    label: string
    action: string
    ok: boolean
    durationMs: number
    error?: string
  }>
  verifies: VerifyResult[]
  /** First failure reason — surfaced at the top of each scenario block. */
  firstFailure?: string
}

export interface RunReport {
  baseUrl: string
  startedAt: string
  finishedAt: string
  durationMs: number
  scenarios: ScenarioReport[]
}

export function summarize(report: RunReport): { passed: number; failed: number; total: number } {
  const passed = report.scenarios.filter((s) => s.ok).length
  const total = report.scenarios.length
  return { passed, failed: total - passed, total }
}

export function writeReport(runDir: string, report: RunReport): void {
  writeFileSync(join(runDir, "results.json"), JSON.stringify(report, null, 2))
  writeFileSync(join(runDir, "report.md"), renderMarkdown(report))
}

function renderMarkdown(report: RunReport): string {
  const { passed, failed, total } = summarize(report)
  const status = failed === 0 ? "✅ PASS" : "❌ FAIL"
  const lines: string[] = []
  lines.push(`# Acceptance Test Run — ${status}`)
  lines.push("")
  lines.push(`- **Target**: ${report.baseUrl}`)
  lines.push(`- **Started**: ${report.startedAt}`)
  lines.push(`- **Duration**: ${(report.durationMs / 1000).toFixed(1)}s`)
  lines.push(`- **Scenarios**: ${passed}/${total} passed${failed > 0 ? ` · **${failed} failed**` : ""}`)
  lines.push("")

  if (failed > 0) {
    lines.push("## Failures")
    lines.push("")
    for (const scenario of report.scenarios) {
      if (scenario.ok) continue
      lines.push(`### ❌ ${scenario.id} — ${scenario.title}`)
      lines.push("")
      lines.push(`*${scenario.section} · \`${scenario.file}\`*`)
      lines.push("")
      if (scenario.firstFailure) {
        lines.push(`**First failure**: ${scenario.firstFailure}`)
        lines.push("")
      }
      const failedVerifies = scenario.verifies.filter((v) => !v.ok)
      if (failedVerifies.length > 0) {
        lines.push("Failed verifies:")
        for (const v of failedVerifies) {
          lines.push(`- \`${v.kind}\`: ${v.reason}`)
        }
        lines.push("")
      }
      const failedSteps = scenario.steps.filter((s) => !s.ok)
      if (failedSteps.length > 0) {
        lines.push("Failed steps:")
        for (const s of failedSteps) {
          lines.push(`- ${s.label} (\`${s.action}\`): ${s.error ?? "non-zero exit"}`)
        }
        lines.push("")
      }
    }
  }

  lines.push("## All scenarios")
  lines.push("")
  lines.push("| Status | ID | Section | Title | Duration |")
  lines.push("|--------|----|---------|-------|----------|")
  for (const s of report.scenarios) {
    const icon = s.ok ? "✅" : "❌"
    lines.push(
      `| ${icon} | ${s.id} | ${s.section} | ${s.title} | ${(s.durationMs / 1000).toFixed(1)}s |`,
    )
  }
  lines.push("")
  return lines.join("\n")
}

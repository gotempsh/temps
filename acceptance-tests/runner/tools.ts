// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Deterministic step executors. Each export takes a parsed step and a run
 * context, runs the action, and returns a `StepResult` capturing what
 * happened (stdout, exit code, screenshot path, etc.).
 *
 * The browser layer wraps the existing `agent-browser` CLI (already
 * installed for /agent-browser workflows) so we don't reinvent a Playwright
 * harness here. Anything `agent-browser` can do, scenarios can do.
 */

import { spawn } from "node:child_process"
import { mkdir } from "node:fs/promises"
import { join } from "node:path"

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface RunContext {
  /** Base URL of the temps web UI being tested (e.g. http://localhost:3000). */
  baseUrl: string
  /** Filesystem path of the per-run results directory. */
  runDir: string
  /** Mutable bag scenarios can read/write — `{{service.id}}` placeholder source. */
  vars: Record<string, unknown>
  /** Where artifacts (screenshots, captured stdout) for this scenario go. */
  scenarioArtifactDir: string
}

export interface StepResult {
  ok: boolean
  /** Captured stdout when relevant (shell, sql, cli). */
  stdout?: string
  /** Captured stderr / error message. */
  stderr?: string
  /** Path to a screenshot if the step took one. */
  screenshotPath?: string
  /** Anything the step wants to expose to verify steps via `${last.*}`. */
  data?: unknown
  /** Wall time in ms. */
  durationMs: number
}

// ---------------------------------------------------------------------------
// Placeholder substitution
//
// Scenarios use `{{service.id}}` style references against `ctx.vars`. We
// resolve these eagerly at step dispatch time so the execution layer never
// has to think about templating.
// ---------------------------------------------------------------------------

const PLACEHOLDER = /\{\{\s*([^}]+?)\s*\}\}/g

export function interpolate(input: string, vars: Record<string, unknown>): string {
  return input.replace(PLACEHOLDER, (_, path: string) => {
    const value = path
      .split(".")
      .reduce<unknown>((acc, key) => (acc == null ? acc : (acc as Record<string, unknown>)[key]), vars)
    if (value == null) {
      throw new Error(`Unresolved placeholder: {{${path}}}`)
    }
    return String(value)
  })
}

// ---------------------------------------------------------------------------
// Process helper — used by every action that shells out (agent-browser,
// docker, psql, bunx). Centralized so timeouts, env, and capture all land
// in one place.
// ---------------------------------------------------------------------------

async function exec(
  argv: string[],
  opts: { timeoutMs?: number; env?: Record<string, string>; stdin?: string } = {},
): Promise<{ stdout: string; stderr: string; code: number }> {
  return new Promise((resolve, reject) => {
    const [cmd, ...args] = argv
    if (!cmd) {
      reject(new Error("exec: empty argv"))
      return
    }
    const child = spawn(cmd, args, {
      env: { ...process.env, ...(opts.env ?? {}) },
      stdio: ["pipe", "pipe", "pipe"],
    })
    let stdout = ""
    let stderr = ""
    child.stdout.on("data", (b) => {
      stdout += b.toString()
    })
    child.stderr.on("data", (b) => {
      stderr += b.toString()
    })
    const timer = opts.timeoutMs
      ? setTimeout(() => {
          child.kill("SIGKILL")
          reject(new Error(`exec timeout: ${argv.join(" ")}`))
        }, opts.timeoutMs)
      : null
    child.on("close", (code) => {
      if (timer) clearTimeout(timer)
      resolve({ stdout, stderr, code: code ?? -1 })
    })
    child.on("error", (e) => {
      if (timer) clearTimeout(timer)
      reject(e)
    })
    if (opts.stdin) {
      child.stdin.write(opts.stdin)
    }
    child.stdin.end()
  })
}

// ---------------------------------------------------------------------------
// Browser actions (agent-browser wrapper)
// ---------------------------------------------------------------------------

async function ab(args: string[]): Promise<{ stdout: string; stderr: string; code: number }> {
  return exec(["agent-browser", ...args], { timeoutMs: 30_000 })
}

export async function uiNavigate(
  step: { url: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const url = step.url.startsWith("http") ? step.url : ctx.baseUrl + step.url
  const r = await ab(["open", interpolate(url, ctx.vars)])
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function uiClick(
  step: { selector: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const r = await ab(["click", interpolate(step.selector, ctx.vars)])
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function uiFill(
  step: { selector: string; value: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const r = await ab([
    "fill",
    interpolate(step.selector, ctx.vars),
    interpolate(step.value, ctx.vars),
  ])
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function uiToggle(
  step: { selector: string; state: "on" | "off" },
  ctx: RunContext,
): Promise<StepResult> {
  // agent-browser exposes check/uncheck for actual checkboxes; many UI
  // toggles are buttons with role=switch and aria-checked. We try check
  // first and fall back to a click when the toggle isn't a true checkbox.
  const t0 = Date.now()
  const cmd = step.state === "on" ? "check" : "uncheck"
  const sel = interpolate(step.selector, ctx.vars)
  let r = await ab([cmd, sel])
  if (r.code !== 0) {
    r = await ab(["click", sel])
  }
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function uiScreenshot(
  step: { name?: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  await mkdir(ctx.scenarioArtifactDir, { recursive: true })
  const filename = `${step.name ?? "screenshot"}-${t0}.png`
  const path = join(ctx.scenarioArtifactDir, filename)
  const r = await ab(["screenshot", path])
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    screenshotPath: path,
    durationMs: Date.now() - t0,
  }
}

export async function uiWaitFor(
  step: { selector: string; timeoutMs?: number },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const r = await ab([
    "waitfor",
    interpolate(step.selector, ctx.vars),
    "--timeout",
    String(step.timeoutMs ?? 10_000),
  ])
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

// ---------------------------------------------------------------------------
// Shell / sql / cli
// ---------------------------------------------------------------------------

export async function shellAction(
  step: { cmd: string; expect_exact?: string; expect_regex?: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const interpolated = interpolate(step.cmd, ctx.vars)
  const r = await exec(["sh", "-c", interpolated], { timeoutMs: 60_000 })
  let ok = r.code === 0
  if (ok && step.expect_exact != null) {
    ok = r.stdout.trim() === step.expect_exact
  }
  if (ok && step.expect_regex != null) {
    ok = new RegExp(step.expect_regex).test(r.stdout)
  }
  return {
    ok,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function sqlAction(
  step: { query: string; database_url?: string },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const url =
    step.database_url ?? process.env.DATABASE_URL ?? "postgres://postgres:password@localhost:5432/temps_development"
  const query = interpolate(step.query, ctx.vars)
  // -A unaligned, -t tuples-only, -F | column separator → easy to parse downstream
  const r = await exec(["psql", url, "-A", "-t", "-F", "|", "-c", query], {
    timeoutMs: 30_000,
  })
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    data: r.stdout
      .trim()
      .split("\n")
      .filter((s) => s.length > 0)
      .map((row) => row.split("|")),
    durationMs: Date.now() - t0,
  }
}

export async function cliAction(
  step: { args: string[] },
  ctx: RunContext,
): Promise<StepResult> {
  const t0 = Date.now()
  const interpolated = step.args.map((a) => interpolate(a, ctx.vars))
  const r = await exec(["bunx", "@temps-sdk/cli", ...interpolated], {
    timeoutMs: 120_000,
  })
  return {
    ok: r.code === 0,
    stdout: r.stdout,
    stderr: r.stderr,
    durationMs: Date.now() - t0,
  }
}

export async function sleepAction(step: { ms: number }): Promise<StepResult> {
  const t0 = Date.now()
  await new Promise((r) => setTimeout(r, step.ms))
  return { ok: true, durationMs: Date.now() - t0 }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

export type StepKind =
  | "ui_navigate"
  | "ui_click"
  | "ui_fill"
  | "ui_toggle"
  | "ui_screenshot"
  | "ui_wait_for"
  | "shell"
  | "sql"
  | "cli"
  | "sleep"

// The scenario YAML schema is permissive on purpose — every action carries
// its own kind-specific fields, narrowed here at dispatch. The double-cast
// (`as unknown as ...`) is deliberate: scenarios are user data, not typed
// inputs, and the per-action functions validate their own shapes.
export async function runStep(
  step: { action: StepKind } & Record<string, unknown>,
  ctx: RunContext,
): Promise<StepResult> {
  const s = step as unknown
  switch (step.action) {
    case "ui_navigate":
      return uiNavigate(s as { url: string }, ctx)
    case "ui_click":
      return uiClick(s as { selector: string }, ctx)
    case "ui_fill":
      return uiFill(s as { selector: string; value: string }, ctx)
    case "ui_toggle":
      return uiToggle(s as { selector: string; state: "on" | "off" }, ctx)
    case "ui_screenshot":
      return uiScreenshot(s as { name?: string }, ctx)
    case "ui_wait_for":
      return uiWaitFor(s as { selector: string; timeoutMs?: number }, ctx)
    case "shell":
      return shellAction(
        s as { cmd: string; expect_exact?: string; expect_regex?: string },
        ctx,
      )
    case "sql":
      return sqlAction(s as { query: string; database_url?: string }, ctx)
    case "cli":
      return cliAction(s as { args: string[] }, ctx)
    case "sleep":
      return sleepAction(s as { ms: number })
    default: {
      const exhaustive: never = step.action
      throw new Error(`Unknown step action: ${exhaustive}`)
    }
  }
}

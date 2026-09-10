// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Verification layer.
 *
 * Two flavors:
 * - Deterministic verifies (`exact`, `regex`, `json_path`, `shell`) run
 *   inline; cheap, no LLM call.
 * - Claude verifies (`claude_assert`) hand a screenshot or text payload to
 *   Sonnet 4.6 with a strict pass/fail tool definition. Prompt caching is
 *   enabled on the system prompt so a 60-scenario run pays once.
 *
 * The judge MUST return a structured verdict via tool use — never free-form
 * text. That keeps the runner deterministic about pass/fail and gives us a
 * structured reason for failure reports.
 */

import Anthropic from "@anthropic-ai/sdk"
import { readFileSync } from "node:fs"

import type { RunContext, StepResult } from "./tools.ts"
import { interpolate } from "./tools.ts"

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type VerifyKind =
  | "exact"
  | "regex"
  | "json_path"
  | "shell"
  | "sql"
  | "claude_assert"

export interface VerifyResult {
  ok: boolean
  /** Human-readable explanation of why this verify passed or failed. */
  reason: string
  /** What kind of verify ran — surfaced in the report for filtering. */
  kind: VerifyKind
  /** Wall time. */
  durationMs: number
}

// ---------------------------------------------------------------------------
// Deterministic verifies
// ---------------------------------------------------------------------------

export function verifyExact(
  spec: { value: string },
  last: StepResult,
): VerifyResult {
  const t0 = Date.now()
  const got = (last.stdout ?? "").trim()
  const ok = got === spec.value
  return {
    ok,
    reason: ok ? `matches '${spec.value}'` : `expected '${spec.value}', got '${got}'`,
    kind: "exact",
    durationMs: Date.now() - t0,
  }
}

export function verifyRegex(
  spec: { pattern: string },
  last: StepResult,
): VerifyResult {
  const t0 = Date.now()
  const got = last.stdout ?? ""
  const re = new RegExp(spec.pattern)
  const ok = re.test(got)
  return {
    ok,
    reason: ok ? `matches /${spec.pattern}/` : `regex /${spec.pattern}/ did not match`,
    kind: "regex",
    durationMs: Date.now() - t0,
  }
}

// ---------------------------------------------------------------------------
// Claude judge
// ---------------------------------------------------------------------------

const JUDGE_SYSTEM_PROMPT = `You are an acceptance-test judge for the Temps deployment platform.

Your job: given a screenshot or captured output and a question about it, return a strict pass/fail verdict via the \`record_verdict\` tool.

Rules:
1. Be conservative. If the evidence does not unambiguously satisfy the question, fail.
2. Always record a reason that quotes specific evidence (the actual toast text, the actual count, the actual button label). Never write "looks fine" or "seems correct".
3. If the screenshot is missing details the question depends on, fail with reason "evidence insufficient: <what is missing>".
4. Never invent details. If you can't see something, say so.

You must call \`record_verdict\` exactly once. Do not respond in plain text.`

const VERDICT_TOOL = {
  name: "record_verdict",
  description: "Record the pass/fail verdict for this verify step.",
  input_schema: {
    type: "object" as const,
    properties: {
      verdict: {
        type: "string" as const,
        enum: ["pass", "fail"],
        description: "pass if the evidence unambiguously satisfies the question, fail otherwise",
      },
      reason: {
        type: "string" as const,
        description:
          "One- or two-sentence explanation citing concrete evidence from the screenshot or text. For failures, name what was expected vs what was observed.",
      },
    },
    required: ["verdict", "reason"],
  },
}

let cachedClient: Anthropic | null = null

function client(): Anthropic {
  if (!cachedClient) {
    if (!process.env.ANTHROPIC_API_KEY) {
      throw new Error("ANTHROPIC_API_KEY is not set; cannot run claude_assert verifies")
    }
    cachedClient = new Anthropic()
  }
  return cachedClient
}

export async function verifyClaudeAssert(
  spec: { prompt: string; screenshot?: string },
  last: StepResult,
  ctx: RunContext,
): Promise<VerifyResult> {
  const t0 = Date.now()

  // Prefer an explicit screenshot reference; otherwise reuse the last step's
  // screenshot if it took one.
  const screenshotPath = spec.screenshot
    ? interpolate(spec.screenshot, ctx.vars)
    : last.screenshotPath

  const userBlocks: Anthropic.ContentBlockParam[] = []

  if (screenshotPath) {
    const data = readFileSync(screenshotPath).toString("base64")
    userBlocks.push({
      type: "image",
      source: { type: "base64", media_type: "image/png", data },
    })
  }

  const interpolated = interpolate(spec.prompt, ctx.vars)
  // Include the last step's stdout when no screenshot is attached so the
  // judge has *something* to look at for shell/sql/cli verifies.
  const trailingContext =
    !screenshotPath && last.stdout ? `\n\nLast step output:\n\n\`\`\`\n${last.stdout.slice(0, 4000)}\n\`\`\`` : ""

  userBlocks.push({
    type: "text",
    text: interpolated + trailingContext,
  })

  const response = await client().messages.create({
    model: "claude-sonnet-4-6",
    max_tokens: 512,
    system: [
      {
        type: "text",
        text: JUDGE_SYSTEM_PROMPT,
        // Cache the static system prompt so a 60-scenario run pays once.
        cache_control: { type: "ephemeral" },
      },
    ],
    tools: [VERDICT_TOOL],
    tool_choice: { type: "tool", name: "record_verdict" },
    messages: [{ role: "user", content: userBlocks }],
  })

  const toolUse = response.content.find((b) => b.type === "tool_use")
  if (!toolUse || toolUse.type !== "tool_use") {
    return {
      ok: false,
      reason: "judge did not return a structured verdict (no tool_use block)",
      kind: "claude_assert",
      durationMs: Date.now() - t0,
    }
  }

  const input = toolUse.input as { verdict?: string; reason?: string }
  const ok = input.verdict === "pass"
  return {
    ok,
    reason: input.reason ?? "(no reason provided)",
    kind: "claude_assert",
    durationMs: Date.now() - t0,
  }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

export interface VerifySpec {
  kind: VerifyKind
  // Each kind has its own additional fields; we narrow at dispatch.
  [key: string]: unknown
}

export async function runVerify(
  spec: VerifySpec,
  last: StepResult,
  ctx: RunContext,
): Promise<VerifyResult> {
  // Mirrors `runStep`: scenario YAML carries kind-specific fields that we
  // narrow at dispatch. Double-cast intentional.
  const s = spec as unknown
  switch (spec.kind) {
    case "exact":
      return verifyExact(s as { value: string }, last)
    case "regex":
      return verifyRegex(s as { pattern: string }, last)
    case "shell": {
      // Run a shell command and pass on exit 0. Mostly a convenience so a
      // verify can call out to docker/psql without redoing the step.
      const t0 = Date.now()
      const { shellAction } = await import("./tools.ts")
      const r = await shellAction(
        s as { cmd: string; expect_exact?: string; expect_regex?: string },
        ctx,
      )
      return {
        ok: r.ok,
        reason: r.ok ? "shell verify passed" : `shell verify failed: ${r.stderr || r.stdout}`,
        kind: "shell",
        durationMs: Date.now() - t0,
      }
    }
    case "sql": {
      // Run a SQL query, then check the joined result against expect_exact
      // or expect_regex. Rows are joined as `col1|col2`, one per line —
      // matches the `psql -A -t -F '|'` format the runner uses.
      const t0 = Date.now()
      const { sqlAction } = await import("./tools.ts")
      const config = s as {
        query: string
        expect_exact?: string
        expect_regex?: string
        database_url?: string
      }
      const r = await sqlAction(
        { query: config.query, database_url: config.database_url },
        ctx,
      )
      const got = (r.stdout ?? "").trim()
      let ok = r.ok
      let reason = r.ok ? `query returned: ${got}` : `query failed: ${r.stderr}`
      if (ok && config.expect_exact != null) {
        ok = got === config.expect_exact
        reason = ok
          ? `matches '${config.expect_exact}'`
          : `expected '${config.expect_exact}', got '${got}'`
      }
      if (ok && config.expect_regex != null) {
        ok = new RegExp(config.expect_regex).test(got)
        reason = ok
          ? `matches /${config.expect_regex}/`
          : `regex /${config.expect_regex}/ did not match: '${got}'`
      }
      return { ok, reason, kind: "sql", durationMs: Date.now() - t0 }
    }
    case "claude_assert":
      return verifyClaudeAssert(s as { prompt: string; screenshot?: string }, last, ctx)
    case "json_path": {
      const t0 = Date.now()
      const { path, equals } = s as { path: string; equals: unknown }
      try {
        const parsed = JSON.parse(last.stdout ?? "null")
        const got = path
          .replace(/^\$\.?/, "")
          .split(".")
          .filter((s) => s.length > 0)
          .reduce<unknown>(
            (acc, key) => (acc == null ? acc : (acc as Record<string, unknown>)[key]),
            parsed,
          )
        const ok = JSON.stringify(got) === JSON.stringify(equals)
        return {
          ok,
          reason: ok
            ? `${path} == ${JSON.stringify(equals)}`
            : `${path} expected ${JSON.stringify(equals)}, got ${JSON.stringify(got)}`,
          kind: "json_path",
          durationMs: Date.now() - t0,
        }
      } catch (e) {
        return {
          ok: false,
          reason: `json_path: failed to parse last stdout as JSON (${(e as Error).message})`,
          kind: "json_path",
          durationMs: Date.now() - t0,
        }
      }
    }
    default: {
      const exhaustive: never = spec.kind
      throw new Error(`Unknown verify kind: ${exhaustive}`)
    }
  }
}

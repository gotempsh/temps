// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// What the reachability banner's one-click Add domain decides from a plan,
// and how it describes the finished run (HTTPS and DNS).

import type { PlanOption, ReachabilityPlan, RunView } from '@/api/client'
import { defaultOption } from '@/lib/reachability'
import { dnsHandling } from '@/lib/reachability-page'

/** What one click on Add domain led to, before the run takes over. */
export type PublishOutcome =
  | { kind: 'started'; run: RunView; option: PlanOption }
  /** The plan changes a choice someone made: say what, and ask here. */
  | { kind: 'confirm'; planId: string; option: PlanOption; message: string }
  /** Nothing the banner can apply: why, in the server's words. */
  | { kind: 'blocked'; reason: string }

/**
 * The per-domain way in from a plan, and whether it can be applied as
 * planned. Only `existing_tunnel` is applied from the banner: it attaches
 * this address alone, never every environment's.
 */
export function publishDecision(
  plan: ReachabilityPlan,
  address: string
):
  | Exclude<PublishOutcome, { kind: 'started' }>
  | { kind: 'apply'; planId: string; option: PlanOption } {
  const option = defaultOption(
    plan.options.filter((o) => o.kind === 'existing_tunnel')
  )
  if (!option)
    return {
      kind: 'blocked',
      reason:
        plan.blocked?.reason ??
        plan.address_error ??
        `No tunnel can publish ${address}.`,
    }
  const missing = option.fields.filter((f) => f.required && f.key !== 'address')
  if (missing.length)
    return {
      kind: 'blocked',
      reason: `Publishing ${address} also needs: ${missing.map((f) => f.label).join(', ')}.`,
    }
  if (!plan.plan_id)
    return {
      kind: 'blocked',
      reason: `The plan for ${address} was not saved. Try again.`,
    }
  // The banner always plans a public address, so an audience step means the
  // address is signed-in or private today and this would open it to anyone:
  // the planner's warning names the address and says so.
  const opens = option.steps.some((s) => s.kind === 'set_audience')
    ? (option.warnings[0] ??
      `${address} is not public now. Adding it here makes it open to anyone.`)
    : null
  const message = [option.confirmation, opens].filter(Boolean).join(' ')
  if (message) return { kind: 'confirm', planId: plan.plan_id, option, message }
  return { kind: 'apply', planId: plan.plan_id, option }
}

/**
 * How visitors get HTTPS and DNS for a run, from its steps: what the
 * banner shows next to the live URL.
 */
export function deliveryStatus(
  run: Pick<RunView, 'steps'>,
  option: Pick<PlanOption, 'kind' | 'way_in' | 'steps' | 'fields'>
): string {
  const step = (kind: string) => run.steps.find((s) => s.kind === kind)
  const cert = step('issue_certificate')
  const https = cert
    ? cert.status === 'done'
      ? 'Certificate issued'
      : cert.status === 'failed'
        ? `Certificate failed${cert.detail ? `: ${cert.detail}` : ''}`
        : 'Certificate pending'
    : option.way_in.startsWith('tunnel:')
      ? cloudflareCovers(option)
        ? 'HTTPS by Cloudflare'
        : "No HTTPS: Cloudflare's free certificate does not cover it"
      : 'HTTPS'
  const record = step('wait_for_publication') ?? step('set_dns_record')
  const dns =
    record?.status === 'done'
      ? 'DNS published'
      : record?.status === 'failed'
        ? `DNS not published${record.detail ? `: ${record.detail}` : ''}`
        : dnsHandling(option).replace(/\.$/, '')
  return `${https} · ${dns}`
}

/**
 * Whether Cloudflare's free certificate covers a tunnel option's address.
 * The planner leaves HTTPS only out of the `http_policy` choices exactly
 * when it does not (a hostname more than one level below the zone).
 */
function cloudflareCovers(option: Pick<PlanOption, 'fields'>): boolean {
  const field = option.fields.find((f) => f.key === 'http_policy')
  if (!field || field.kind.type !== 'choice') return true
  return field.kind.choices.some((c) => c.value === 'https_only')
}

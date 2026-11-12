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
  if (option.confirmation)
    return {
      kind: 'confirm',
      planId: plan.plan_id,
      option,
      message: option.confirmation,
    }
  return { kind: 'apply', planId: plan.plan_id, option }
}

/**
 * How visitors get HTTPS and DNS for a run, from its steps: what the
 * banner shows next to the live URL.
 */
export function deliveryStatus(
  run: Pick<RunView, 'steps'>,
  option: Pick<PlanOption, 'kind' | 'way_in' | 'steps'>
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
      ? 'HTTPS by Cloudflare'
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

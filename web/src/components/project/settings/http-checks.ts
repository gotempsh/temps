// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  listHttpChecks,
  type ExpiringArtifact,
  type HttpCheckView,
  type VariableHistoryDetails,
} from '@/api/client'

/** The stored credential a check reads: an environment variable or a secret. */
export interface CredentialSubject {
  kind: 'env_var' | 'secret'
  id: number
  key: string
}

/** Provider recorded on checks created automatically from a detected expiring item. */
export const LOCAL_PROVIDER = 'local_expiry'
export const DEFAULT_WARNING_DAYS = [30, 7, 1]
/** What a local expiry check can read, for onboarding copy. */
export const LOCAL_FORMATS = [
  'X.509 certificates',
  'SSH certificates',
  'OpenPGP keys',
  'kubeconfigs',
  'JWTs',
] as const

/** "Certificate 'svc.example.test' · expires 2027-01-04" */
export function describeArtifact(
  artifact: Pick<ExpiringArtifact, 'label' | 'expires_at'>
) {
  return `${artifact.label} · expires ${artifact.expires_at.slice(0, 10)}`
}

export const httpChecksKey = (projectId: number) => ['http-checks', projectId]
export function useHttpChecks(projectId: number) {
  return useQuery({
    queryKey: httpChecksKey(projectId),
    queryFn: async () => {
      const items: HttpCheckView[] = []
      let page = 1
      while (true) {
        const response = await listHttpChecks({
          path: { project_id: projectId },
          query: { page, page_size: 100 },
          throwOnError: true,
        })
        items.push(...response.data.items)
        if (
          items.length >= response.data.total ||
          response.data.items.length === 0
        )
          return items
        page++
      }
    },
    refetchInterval: 10_000,
  })
}

export function checksFor(
  checks: readonly HttpCheckView[],
  subject: Pick<CredentialSubject, 'kind' | 'id'>
) {
  return checks.filter((check) =>
    subject.kind === 'secret'
      ? check.secret_id === subject.id
      : check.env_var_id === subject.id
  )
}

/** Request fields that bind a new check to its subject; a check has one source. */
export function credentialSource(subject: CredentialSubject) {
  return {
    env_var_id: subject.kind === 'env_var' ? subject.id : null,
    secret_id: subject.kind === 'secret' ? subject.id : null,
    credential: null,
  }
}

/** Groups check indicators by the subject they read, for list pages. */
export function indicatorsBySubject(
  checks: readonly HttpCheckView[],
  kind: CredentialSubject['kind']
) {
  const map = new Map<number, ReturnType<typeof checkIndicators>>()
  for (const check of checks) {
    const id = kind === 'secret' ? check.secret_id : check.env_var_id
    if (id == null) continue
    map.set(id, [...(map.get(id) ?? []), ...checkIndicators([check])])
  }
  return map
}

/**
 * Parses "30, 7, 1" into warning thresholds, mirroring the server's rule:
 * 1–8 distinct whole days between 1 and 365. Returns null when invalid.
 */
export function parseWarningDays(input: string): number[] | null {
  const parts = input
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean)
  if (!parts.length || parts.some((part) => !/^\d+$/.test(part))) return null
  const days = [...new Set(parts.map(Number))].sort((a, b) => b - a)
  if (days.length > 8 || days.some((day) => day < 1 || day > 365)) return null
  return days
}

export function checkSourceLabel(
  check: Pick<HttpCheckView, 'automatic_provider' | 'kind'>
) {
  if (check.automatic_provider === LOCAL_PROVIDER)
    return 'Automatic expiry check'
  if (check.automatic_provider) return 'Automatic detection'
  return check.kind === 'local' ? 'Local expiry check' : 'Custom HTTP check'
}

const nouns = { env_var: 'Variable', secret: 'Secret' } as const
export function subjectNoun(kind: CredentialSubject['kind']) {
  return nouns[kind]
}

/** Display name for a variable or secret history event kind. */
export function historyEventName(
  kind: string,
  subject: CredentialSubject['kind']
) {
  const noun = nouns[subject]
  const names: Record<string, string> = {
    tracking_started: 'History tracking started',
    detection_unavailable:
      'Credential could not be read for automatic detection',
    created: `${noun} created`,
    value_changed: 'Value rotated',
    settings_changed: `${noun} settings updated`,
    scope_changed: 'Access scope changed',
    check_added: 'Check added',
    check_updated: 'Check updated',
    check_removed: 'Check removed',
    check_paused: 'Check paused',
    check_resumed: 'Check resumed',
    verification: 'Verification completed',
  }
  return names[kind] ?? `${noun} activity`
}

/** Human summary of a secret's access scope recorded by a `scope_changed` event. */
export function scopeSummary(
  details: Pick<VariableHistoryDetails, 'environment_ids' | 'compose_services'>
) {
  if (details.environment_ids == null && details.compose_services == null)
    return null
  const environments = details.environment_ids ?? []
  const services = details.compose_services ?? []
  const scope = environments.length
    ? `${environments.length} ${environments.length === 1 ? 'environment' : 'environments'}`
    : 'All environments'
  return services.length ? `${scope} · only ${services.join(', ')}` : scope
}

/** The provider mark for a check: every local expiry check, manual or automatic, gets the local mark. */
export function markProvider(
  check: Pick<HttpCheckView, 'kind' | 'automatic_provider'>
) {
  return check.kind === 'local' ? LOCAL_PROVIDER : check.automatic_provider
}

export function checkIndicators(checks: HttpCheckView[]) {
  return checks.map((check) => ({
    id: String(check.id),
    provider: markProvider(check),
    status: !check.enabled
      ? ('unknown' as const)
      : (check.result?.status ?? ('pending' as const)),
    label: !check.enabled
      ? `${check.name}: paused`
      : check.result
        ? `${check.name}: ${check.result.status}`
        : `${check.name}: awaiting check`,
    detail: !check.enabled
      ? 'Scheduled checks are paused.'
      : check.result
        ? `${check.result.findings.map((finding) => finding.message).join(' ')} Last checked ${new Date(check.result.checked_at).toLocaleString()}.`
        : 'The first check has not completed yet.',
  }))
}

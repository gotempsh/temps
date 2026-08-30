// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface SecretDraft {
  key: string
  value: string
  scope: 'production' | 'preview' | 'all'
}

export interface SecretReference {
  key: string
  reference: string
  scope: SecretDraft['scope']
  status: 'stored'
}

const CREDENTIAL_PATTERNS = [
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/i,
  /\b(?:sk|pk)_(?:live|test)_[a-z0-9_-]{12,}\b/i,
  /\bgh[oprsu]_[a-z0-9]{20,}\b/i,
  /\b(?:api[_-]?key|password|passwd|secret|token)\s*[:=]\s*[^\s]{8,}/i,
  /(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?):\/\/[^\s:@]+:[^\s@]+@/i,
] as const

/**
 * Fast client-side guard for the prototype composer. It is deliberately
 * conservative: suspicious input is diverted to the secret broker before any
 * chat request can be made. The server must repeat this check in a real build.
 */
export function containsLikelyCredential(input: string): boolean {
  return CREDENTIAL_PATTERNS.some((pattern) => pattern.test(input))
}

function normalizeSecretKey(key: string): string {
  return key
    .trim()
    .toUpperCase()
    .replace(/[^A-Z0-9_]/g, '_')
}

/**
 * Converts plaintext browser input into the only representation exposed to
 * the model. Values are intentionally excluded from the returned shape.
 */
export function buildSecretReferencePayload(
  projectSlug: string,
  drafts: SecretDraft[]
): SecretReference[] {
  return drafts.map((draft) => {
    const key = normalizeSecretKey(draft.key)
    return {
      key,
      reference: `secret://projects/${projectSlug}/${draft.scope}/${key}`,
      scope: draft.scope,
      status: 'stored',
    }
  })
}

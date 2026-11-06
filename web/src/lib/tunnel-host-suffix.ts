// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A tunnel's hostname suffix: environments it publishes are named
// `<label>-<suffix>.<zone>`, one level below the zone, which Cloudflare's
// free certificate covers (`<label>.<sub>.<zone>` is two levels down).

/** `temps_ingress::reachability::environment_hosts::MAX_HOST_SUFFIX_LEN`. */
export const MAX_HOST_SUFFIX_LEN = 20
const HOST_SUFFIX = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/

/** The suffix as the server stores it: trimmed and lowercase. */
export function normalizeHostSuffix(suffix: string): string {
  return suffix.trim().toLowerCase()
}

/**
 * Why a hostname suffix is unusable, or `undefined` when it is fine: one DNS
 * label fragment, as `normalize_host_suffix` checks on the server.
 */
export function hostSuffixError(suffix: string): string | undefined {
  const value = normalizeHostSuffix(suffix)
  if (!value) return 'Enter the label, for example lab.'
  if (value.length > MAX_HOST_SUFFIX_LEN || !HOST_SUFFIX.test(value)) {
    return `Use 1-${MAX_HOST_SUFFIX_LEN} letters, digits or hyphens, with no dots.`
  }
  return undefined
}

/** `<label>-<suffix>.<domain>`, or `<label>.<domain>` without a suffix. */
export function suffixedHost(
  label: string,
  suffix: string | null | undefined,
  domain: string
): string {
  const value = suffix ? normalizeHostSuffix(suffix) : ''
  return value ? `${label}-${value}.${domain}` : `${label}.${domain}`
}

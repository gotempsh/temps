// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Public ingress (ADR-051): the base URL Temps hands to third parties that
 * call back into it. It is not necessarily the address the operator is
 * browsing — that one is the console and may be restricted to an allowlist —
 * so a callback URL shown in the console must be built from the API's answer,
 * never from `window.location.origin`.
 */

/** The part of `GET /settings/public-ingress` URL building needs. */
export interface PublicIngressUrlSource {
  effective_url?: string | null
}

/**
 * Base URL for anything a third party will request, with no trailing slash,
 * or `null` while it is not known.
 *
 * There is deliberately no fallback to the browser's origin. When a separate
 * public ingress URL is configured, the origin is the private console: a
 * webhook registered against it can never be delivered, and nothing reports
 * that. A caller that has no URL yet must wait or offer a retry instead.
 */
export function publicIngressBaseUrl(
  status: PublicIngressUrlSource | null | undefined
): string | null {
  const configured = status?.effective_url?.trim().replace(/\/+$/, '')
  return configured || null
}

/** Join an ingress base URL and an absolute path such as `/api/webhook/...`. */
export function publicIngressCallbackUrl(
  baseUrl: string,
  path: string
): string {
  const base = baseUrl.replace(/\/+$/, '')
  return `${base}${path.startsWith('/') ? path : `/${path}`}`
}

/**
 * The callback URL for `path`, or `null` while the ingress URL is not known.
 * A `null` must disable whatever would copy or submit the URL.
 */
export function publicIngressCallbackUrlOrNull(
  status: PublicIngressUrlSource | null | undefined,
  path: string
): string | null {
  const base = publicIngressBaseUrl(status)
  return base === null ? null : publicIngressCallbackUrl(base, path)
}

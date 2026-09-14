// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function safePreviewHost(url: string | null): string | null {
  if (!url) return null
  try {
    return new URL(url).host
  } catch {
    return null
  }
}

export function previewAccessRequest(params: URLSearchParams) {
  const sandbox = params.get('sandbox') ?? ''
  const port = Number(params.get('port'))
  const path = params.get('path') ?? '/'
  if (
    !/^sbx_[a-f0-9]{16,64}$/.test(sandbox) ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535
  )
    return null
  if (!safePreviewPath(path)) return null
  return { sandbox_public_id: sandbox, port, path }
}

export function safePreviewPath(path: unknown): path is string {
  return (
    typeof path === 'string' &&
    path.length <= 8192 &&
    path.startsWith('/') &&
    !path.startsWith('//') &&
    !/[\\\p{Cc}]/u.test(path)
  )
}

/** Unconfigured gateways signal expiry without disclosing their current URL. */
export function previewRenewalPath(
  data: unknown,
  currentPath: string
): string | null {
  if (!data || typeof data !== 'object') return null
  const message = data as { type?: unknown; path?: unknown }
  if (message.type !== 'temps:preview-auth-required') return null
  const path = message.path === undefined ? currentPath : message.path
  return safePreviewPath(path) ? path : null
}

export function previewErrorMessage(error: unknown): string {
  if (error && typeof error === 'object') {
    const payload = error as {
      detail?: unknown
      title?: unknown
      message?: unknown
    }
    for (const candidate of [payload.detail, payload.title, payload.message]) {
      if (typeof candidate === 'string' && candidate.trim()) return candidate
    }
  }
  return 'Temps could not open this sandbox preview.'
}

/** Explain a failed cookie exchange without claiming the app itself is down. */
export function previewCookieErrorMessage(previewUrl: string): string {
  try {
    if (new URL(previewUrl).protocol === 'http:') {
      return 'Embedded preview authentication could not be retained. This preview uses HTTP, which cannot send authentication cookies in a cross-site iframe. Open it in a new tab, or configure HTTPS for the console and preview domain to embed it here.'
    }
  } catch {
    // Do not echo a malformed URL: it may contain a short-lived grant.
  }
  return 'Your browser did not retain the embedded preview authentication cookie. Open the preview in a new tab, or check browser cookie restrictions for this site.'
}

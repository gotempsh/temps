// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Minimal Sentry-compatible event payload builder + a raw sender for
 * `POST /{project_id}/store/`.
 *
 * This endpoint's generated SDK type (`SentryEventRequest`) only declares
 * `event_id`/`message`/`platform`/`timestamp` -- the real handler takes
 * `Json<serde_json::Value>` and expects a full Sentry event shape
 * (`exception.values[].{type,value,stacktrace}`), so the OpenAPI schema is
 * decorative only. Sending the realistic payload real SDKs actually produce
 * needs a raw fetch, not the typed client.
 */

export interface SentryStackFrame {
  filename: string
  function: string
  lineno: number
  colno: number
}

export interface SentryEventOptions {
  exceptionType: string
  exceptionValue: string
  frames: SentryStackFrame[]
  environment?: string
  release?: string
}

/** A fresh, valid 32-hex-char event_id (Sentry's UUID-without-dashes convention). */
export function makeSentryEventId(): string {
  return crypto.randomUUID().replace(/-/g, '')
}

export function buildSentryEvent(opts: SentryEventOptions): Record<string, unknown> {
  return {
    event_id: makeSentryEventId(),
    timestamp: Date.now() / 1000,
    platform: 'node',
    level: 'error',
    exception: {
      values: [
        {
          type: opts.exceptionType,
          value: opts.exceptionValue,
          stacktrace: { frames: opts.frames },
        },
      ],
    },
    environment: opts.environment ?? 'production',
    release: opts.release ?? 'e2e-test',
  }
}

/** POST a Sentry event JSON payload to `/{project_id}/store/` using DSN auth (not the bearer token). */
export async function sendSentryEvent(opts: {
  apiBaseUrl: string
  projectId: number
  dsnPublicKey: string
  event: Record<string, unknown>
}): Promise<{ status: number; body: unknown }> {
  const res = await fetch(`${opts.apiBaseUrl}/${opts.projectId}/store/`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-Sentry-Auth': `Sentry sentry_key=${opts.dsnPublicKey}`,
    },
    body: JSON.stringify(opts.event),
  })
  let body: unknown
  try {
    body = await res.json()
  } catch {
    body = await res.text().catch(() => undefined)
  }
  return { status: res.status, body }
}

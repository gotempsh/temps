// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Patterns that indicate a value should be masked based on the KEY name.
const SENSITIVE_PATTERNS = [
  /secret/i,
  /password/i,
  /token/i,
  /api[_-]?key/i,
  /auth/i,
  /credential/i,
  /private[_-]?key/i,
  /access[_-]?key/i,
  /sentry[_-]?dsn/i,
  /database[_-]?url/i,
  /connection[_-]?string/i,
  /jwt/i,
  /bearer/i,
  /postgres[_-]?url/i,
  /mysql[_-]?url/i,
  /redis[_-]?url/i,
  /mongodb?[_-]?url/i,
  /amqp[_-]?url/i,
  /clickhouse[_-]?url/i,
  /dsn/i,
  /webhook[_-]?url/i,
  /signing[_-]?key/i,
]

/**
 * Check if a key name suggests the value should be masked.
 */
export function shouldMaskValue(key: string): boolean {
  return SENSITIVE_PATTERNS.some((pattern) => pattern.test(key))
}

// Connection-string scheme followed by userinfo. Captures:
//   1: scheme://user
//   2: password (with anything except @ and whitespace, to keep the regex
//      anchored against the closest @host segment)
// Common schemes we want to redact: postgres(ql), mysql, redis(s), mongodb,
// mongodb+srv, amqp(s), rediss, clickhouse(s), https-with-basic-auth, etc.
const URL_WITH_USERINFO =
  /\b([a-zA-Z][a-zA-Z0-9+.-]*:\/\/[^\s/?#:@]+):([^\s@]+)@/g

// Authorization: Bearer <token> / Authorization=Bearer <token>. Captures the
// scheme keyword so we can keep it visible (it's not the secret); the token
// after it is what we redact.
const BEARER_TOKEN_RE =
  /\b(Authorization\s*[:=]\s*[A-Za-z]+\s+|[A-Za-z][A-Za-z0-9-]*\s+)([A-Za-z0-9._\-+/=]{16,})/g

// Standalone JWT shape: header.payload.signature, base64url tokens of any
// length but conventionally 16+ chars each. Conservative on character set so
// we don't match three-dotted version strings (e.g. 1.2.3) — bumping the
// minimum size on each segment is what keeps this from over-matching.
const JWT_RE = /\b([A-Za-z0-9_-]{8,})\.([A-Za-z0-9_-]{8,})\.([A-Za-z0-9_-]{8,})\b/g

const SENSITIVE_VALUE_PATTERNS = [
  URL_WITH_USERINFO,
  /Authorization\s*[:=]\s*[A-Za-z]+\s+[A-Za-z0-9._\-+/=]{16,}/i,
  JWT_RE,
]

/**
 * Check if a value LOOKS like it contains a secret regardless of its key
 * name. Catches things like `OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer
 * dt_…` where the key wouldn't trip shouldMaskValue but the value clearly
 * carries credentials.
 *
 * We test against fresh copies of the patterns because the module-level
 * regexes carry the /g flag and global regexes are stateful between calls.
 */
export function shouldMaskValueByContent(value: string): boolean {
  if (!value) return false
  return SENSITIVE_VALUE_PATTERNS.some((pattern) => {
    const fresh = new RegExp(pattern.source, pattern.flags)
    return fresh.test(value)
  })
}

/**
 * Mask a value with bullet points, showing only last 4 characters. Used when
 * the entire value is a single opaque secret (password, raw API key).
 */
export function maskValue(value: string): string {
  if (!value || value.length <= 4) {
    return '••••••••'
  }
  const visiblePart = value.slice(-4)
  const maskedLength = Math.min(value.length - 4, 20)
  return '•'.repeat(maskedLength) + visiblePart
}

function bullets(n: number): string {
  return '•'.repeat(Math.max(4, Math.min(n, 20)))
}

/**
 * Redact only the secret-bearing parts of a value, leaving structure intact
 * so the user can still recognise/debug it. Examples:
 *   postgresql://u:p@h:5432/db -> postgresql://u:••••••••@h:5432/db
 *   Authorization=Bearer dt_xyz -> Authorization=Bearer •••xyz
 *   header.payload.sig -> header.•••••••.•••sig
 * Anything we can't recognise as a secret structure is returned unchanged —
 * callers fall back to `maskValue()` for the "whole value is the secret"
 * case via the key-name heuristic.
 */
export function maskCredentialsInValue(value: string): string {
  if (!value) return value
  let out = value

  out = out.replace(URL_WITH_USERINFO, (_match, prefix: string, secret: string) => {
    return `${prefix}:${bullets(secret.length)}@`
  })

  out = out.replace(
    BEARER_TOKEN_RE,
    (match, prefix: string, token: string) => {
      // Heuristic: only redact when the prefix actually says Authorization
      // or names a known scheme. Avoids mangling "log line 42 stack traces"
      // that happen to have a long word after them.
      if (!/Authorization|Bearer|Token|Basic|ApiKey|Api-Key/i.test(prefix)) {
        return match
      }
      const tail = token.length > 6 ? token.slice(-4) : ''
      return `${prefix}${bullets(token.length - tail.length)}${tail}`
    },
  )

  out = out.replace(
    JWT_RE,
    (_match, header: string, payload: string, signature: string) => {
      const sigTail = signature.length > 6 ? signature.slice(-4) : ''
      return `${header}.${bullets(payload.length)}.${bullets(
        signature.length - sigTail.length,
      )}${sigTail}`
    },
  )

  return out
}

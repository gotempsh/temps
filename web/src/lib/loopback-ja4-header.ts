// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * The header a trusted loopback proxy sends the visitor's JA4 in (ADR 066).
 * The same rule the settings API applies: blank for none, or an HTTP token
 * (RFC 9110) containing "ja4". The proxy strips this header from every
 * request, so the rule keeps it from ever stripping one an app needs.
 */
export const loopbackJa4HeaderSchema = z
  .string()
  .trim()
  .refine(
    (value) => value === '' || /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(value),
    {
      message: 'Not a valid header name: no spaces, colons or other separators',
    }
  )
  .refine((value) => value === '' || value.toLowerCase().includes('ja4'), {
    message: 'The header name must contain "ja4", for example x-ja4',
  })

/** react-hook-form `validate`: `true`, or the message to show by the field. */
export function validateLoopbackJa4Header(value: string): true | string {
  const result = loopbackJa4HeaderSchema.safeParse(value)
  return result.success || (result.error.issues[0]?.message ?? 'Invalid header')
}

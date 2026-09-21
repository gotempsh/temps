// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { enrichVisitor } from '../../api/sdk.gen.js'
import type { EnrichVisitorResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  newline,
  json as jsonOut,
  success,
  info,
  keyValue,
} from '../../ui/output.js'

export interface EnrichOptions {
  data?: string
  file?: string
  set?: string[]
  unset?: string[]
  json?: boolean
}

/**
 * Everything `buildEnrichPayload` needs, with the file already read.
 *
 * The action does the I/O so the payload rules stay a pure function: the
 * precedence between four flags is the part worth testing, and it should not
 * need a filesystem to be tested.
 */
export interface EnrichPayloadInput {
  /** Raw text of `--data`. */
  data?: string
  /** Contents of the `--file` path (already read from disk). */
  fileContents?: string
  /** The `--file` path, used only in error messages. */
  filePath?: string
  /** Repeated `--set key=value` pairs. */
  set?: string[]
  /** Repeated `--unset key` names. */
  unset?: string[]
}

export type BuildEnrichPayloadResult =
  | {
      ok: true
      /** The `custom_data` object sent to the API. */
      payload: Record<string, unknown>
      /** Keys this call writes, in payload order. */
      setKeys: string[]
      /** Keys this call removes (sent as `null`), in payload order. */
      unsetKeys: string[]
    }
  | { ok: false; error: string }

/** Human name for what the user actually passed, for a "that isn't an object" error. */
function describeJsonKind(value: unknown): string {
  if (value === null) return 'null'
  if (Array.isArray(value)) return 'an array'
  switch (typeof value) {
    case 'string':
      return 'a string'
    case 'number':
      return 'a number'
    case 'boolean':
      return 'a boolean'
    default:
      return `a ${typeof value}`
  }
}

/**
 * Parse one JSON source (`--data` or `--file`) and insist it is an object.
 *
 * `custom_data` is merged key-by-key into the visitor row, so an array or a
 * bare value has nothing to merge — the server rejects it with a 400, and
 * failing here says which flag was wrong instead.
 */
function parseJsonObject(
  raw: string,
  flagLabel: string,
): { ok: true; value: Record<string, unknown> } | { ok: false; error: string } {
  const trimmed = raw.trim()
  if (trimmed === '') {
    return {
      ok: false,
      error: `${flagLabel} is empty. Pass a JSON object, e.g. '{"user_id":"user_123"}'.`,
    }
  }

  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e)
    return {
      ok: false,
      error: `${flagLabel} is not valid JSON: ${detail}. Expected a JSON object, e.g. '{"user_id":"user_123"}'.`,
    }
  }

  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return {
      ok: false,
      error: `${flagLabel} must be a JSON object; got ${describeJsonKind(parsed)}. Enrichment merges top-level keys, so it needs an object, e.g. '{"user_id":"user_123"}'.`,
    }
  }

  return { ok: true, value: parsed as Record<string, unknown> }
}

/**
 * Build the `custom_data` body from the four input flags.
 *
 * Precedence, lowest to highest: `--file`, `--data`, `--set`, `--unset`. Later
 * occurrences of a repeatable flag win over earlier ones. `--unset` is last on
 * purpose: removing a key is the destructive operation, so it is never
 * silently overwritten by a key that happens to appear in the JSON as well.
 *
 * A key given to BOTH `--set` and `--unset` is rejected rather than resolved:
 * writing it and removing it in the same call is a typo, not an intent.
 */
export function buildEnrichPayload(
  input: EnrichPayloadInput,
): BuildEnrichPayloadResult {
  const setPairs = input.set ?? []
  const unsetKeys = input.unset ?? []

  if (
    input.data === undefined &&
    input.fileContents === undefined &&
    setPairs.length === 0 &&
    unsetKeys.length === 0
  ) {
    return {
      ok: false,
      error:
        'Nothing to enrich. Pass --data \'{"user_id":"user_123"}\', --file <path>, --set key=value, or --unset <key>.',
    }
  }

  const payload: Record<string, unknown> = {}

  if (input.fileContents !== undefined) {
    const parsed = parseJsonObject(
      input.fileContents,
      input.filePath ? `--file "${input.filePath}"` : '--file',
    )
    if (!parsed.ok) return parsed
    Object.assign(payload, parsed.value)
  }

  if (input.data !== undefined) {
    const parsed = parseJsonObject(input.data, '--data')
    if (!parsed.ok) return parsed
    Object.assign(payload, parsed.value)
  }

  const explicitSetKeys: string[] = []
  for (const pair of setPairs) {
    const eqIdx = pair.indexOf('=')
    if (eqIdx === -1) {
      // No "=" means no value to leak: echoing what was typed is safe here.
      return {
        ok: false,
        error: `Invalid --set "${pair}". Expected format: key=value (values are sent as strings; use --data for numbers, booleans, or nested values).`,
      }
    }
    const key = pair.slice(0, eqIdx).trim()
    if (key === '') {
      // Deliberately does NOT echo the pair: the value may be personal data.
      return {
        ok: false,
        error:
          'Invalid --set: the key before "=" cannot be empty. Expected format: key=value.',
      }
    }
    payload[key] = pair.slice(eqIdx + 1)
    if (!explicitSetKeys.includes(key)) explicitSetKeys.push(key)
  }

  for (const raw of unsetKeys) {
    const key = raw.trim()
    if (key === '') {
      return {
        ok: false,
        error:
          'Invalid --unset: the key name cannot be empty. Pass the top-level key to remove, e.g. --unset trial_ends_at.',
      }
    }
    if (key.includes('=')) {
      const keyPart = key.slice(0, key.indexOf('='))
      return {
        ok: false,
        error: `Invalid --unset "${keyPart}=...": --unset takes a key name only and sends null to remove it. Use --set ${keyPart}=<value> to write a value instead.`,
      }
    }
    if (explicitSetKeys.includes(key)) {
      return {
        ok: false,
        error: `Key "${key}" is in both --set and --unset. Pick one: --set writes the key, --unset removes it.`,
      }
    }
    payload[key] = null
  }

  const allKeys = Object.keys(payload)

  if (allKeys.length === 0) {
    return {
      ok: false,
      error:
        'Nothing to enrich: the JSON object has no keys. Add at least one key, or use --unset <key> to remove one.',
    }
  }

  // A `null` is a removal wherever it came from — `--unset`, or a literal
  // `null` typed into `--data`/`--file`. Reporting them together keeps the
  // summary honest about what the server will do.
  return {
    ok: true,
    payload,
    setKeys: allKeys.filter((key) => payload[key] !== null),
    unsetKeys: allKeys.filter((key) => payload[key] === null),
  }
}

/**
 * The `--json` payload.
 *
 * Carries the server's own `success`/`message` verbatim (a caller scripting
 * against this needs the real outcome, not a rephrasing) plus the key names
 * this call wrote and removed. Key names only: the values are routinely
 * personal data and the user already has them.
 */
export function enrichJsonOutput(
  visitorIdArg: string,
  response: EnrichVisitorResponse,
  built: { setKeys: string[]; unsetKeys: string[] },
): Record<string, unknown> {
  return {
    requested_visitor_id: visitorIdArg,
    visitor_id: response.visitor_id,
    success: response.success,
    message: response.message,
    set_keys: built.setKeys,
    unset_keys: built.unsetKeys,
  }
}

/**
 * The failure text for `success: false` (HTTP 200) — the API's way of saying
 * "no such visitor", which must not read like a successful enrichment.
 */
export function visitorNotFoundMessage(
  visitorIdArg: string,
  serverMessage: string,
): string {
  return (
    `${serverMessage}: no visitor matched "${visitorIdArg}", so nothing was enriched. ` +
    'Check the ID is current (sealed enc_... IDs come from the _temps_visitor_id cookie, ' +
    'and a visitor only exists once it has sent at least one event), and that the ' +
    'credential you are using can see that visitor\'s project.'
  )
}

export async function enrichVisitorAction(
  visitorIdArg: string,
  options: EnrichOptions,
): Promise<void> {
  const visitorId = visitorIdArg.trim()
  if (visitorId === '') {
    throw new Error(
      'Visitor ID is required. Pass a numeric visitor ID, a visitor GUID, or the sealed enc_... ID from the _temps_visitor_id cookie.',
    )
  }

  let fileContents: string | undefined
  if (options.file !== undefined) {
    try {
      fileContents = await Bun.file(options.file).text()
    } catch (e) {
      const detail = e instanceof Error ? e.message : String(e)
      throw new Error(
        `Could not read --file "${options.file}": ${detail}. Point --file at a readable file containing a JSON object.`,
      )
    }
  }

  const built = buildEnrichPayload({
    data: options.data,
    fileContents,
    filePath: options.file,
    set: options.set,
    unset: options.unset,
  })

  if (!built.ok) {
    throw new Error(built.error)
  }

  await requireAuth()
  await setupClient()

  const response = await withSpinner('Enriching visitor...', async () => {
    const { data, error } = await enrichVisitor({
      client,
      path: { visitor_id: visitorId },
      body: { custom_data: built.payload },
    })

    if (error) throw new Error(getErrorMessage(error))
    if (!data) {
      throw new Error(
        `Enrich request for visitor "${visitorId}" returned no response body. Re-run with DEBUG=1, and check the Temps instance is reachable.`,
      )
    }
    return data
  })

  if (options.json) {
    jsonOut(enrichJsonOutput(visitorIdArg, response, built))
  }

  // `success: false` arrives as HTTP 200 — treat it as the failure it is, so a
  // script that pipes this command never mistakes "no such visitor" for a
  // write that happened.
  if (!response.success) {
    throw new Error(visitorNotFoundMessage(visitorIdArg, response.message))
  }

  if (options.json) return

  newline()
  success(response.message)
  keyValue('Visitor', response.visitor_id)
  if (built.setKeys.length > 0) {
    keyValue('Keys set', built.setKeys.join(', '))
  }
  if (built.unsetKeys.length > 0) {
    keyValue('Keys removed', built.unsetKeys.join(', '))
  }
  newline()
  info(
    'Enrichment merges into the visitor\'s existing custom_data; keys you did not send are left alone.',
  )
}

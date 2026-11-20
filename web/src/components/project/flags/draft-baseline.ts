// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'

/** The stored state a draft was seeded from, and the revision it had. */
interface Baseline<T> {
  revision: string | null
  key: string
  fields: T
}

/** What to do when the stored revision moves past a draft's baseline. */
export type BaselineStep = 'keep' | 'advance' | 'reseed'

/**
 * Decide how a draft follows a refetch, without ever saving over someone
 * else's change:
 *
 * - `advance`: the fields this draft writes are unchanged on the server (the
 *   revision moved for another field), or the draft already equals the new
 *   stored fields (e.g. our own save landed). Adopt the new revision.
 * - `reseed`: the fields changed but the draft has no unsaved edits. Take the
 *   server's version.
 * - `keep`: the fields changed under unsaved edits. Keep the edits and the
 *   old revision, so saving them is refused with 409 instead of overwriting.
 */
export function baselineStep<T>(
  base: Baseline<T>,
  key: string,
  fields: T,
  differsFrom: (fields: T) => boolean
): BaselineStep {
  if (base.key === key || !differsFrom(fields)) return 'advance'
  if (!differsFrom(base.fields)) return 'reseed'
  return 'keep'
}

/**
 * Track the revision a draft is based on across refetches, so an editor can
 * stay mounted (unsaved edits survive) and still send the right
 * `expected_updated_at`. `differsFrom(fields)` says whether the current draft
 * differs from the given stored fields; `reseed(fields)` replaces the draft.
 * Returns the revision to send with a save.
 */
export function useDraftBaseline<T>(
  revision: string | null,
  fields: T,
  differsFrom: (fields: T) => boolean,
  reseed: (fields: T) => void
): string | null {
  const key = JSON.stringify(fields) ?? ''
  const [base, setBase] = useState<Baseline<T>>(() => ({
    revision,
    key,
    fields,
  }))
  if (base.revision !== revision) {
    const step = baselineStep(base, key, fields, differsFrom)
    // Adjusting state while rendering, the documented alternative to an
    // effect: React re-renders this component before committing.
    if (step !== 'keep') setBase({ revision, key, fields })
    if (step === 'reseed') reseed(fields)
  }
  return base.revision
}

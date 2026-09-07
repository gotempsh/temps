// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for detecting a stalled Temps Cloud telemetry backfill.
 *
 * "Stalled" means the process driving the backfill has gone quiet for long
 * enough that it has almost certainly exited. The bar would otherwise spin
 * forever with no explanation; calling it stalled lets the operator know they
 * can re-run the command (the backfill is resumable and idempotent).
 */

import type { CloudBackfillStatusResponse } from '@/api/client/types.gen'

/**
 * How long a `running` backfill may go without a progress write before the UI
 * calls it stalled.
 *
 * The CLI writes once per chunk, and a chunk is a day of spans, so a few
 * minutes of silence is normal and ten is not. Calling it stalled is better
 * than a spinner that never resolves: the operator can go and look at the
 * terminal, or just re-run — the backfill is resumable and idempotent.
 */
export const BACKFILL_STALL_THRESHOLD_MS = 10 * 60 * 1000

/**
 * Whether a `running` backfill has gone quiet for long enough that the process
 * driving it is probably gone.
 */
export function isBackfillStalled(
  status: CloudBackfillStatusResponse | undefined,
  now: number = Date.now()
): boolean {
  if (!status || status.status !== 'running' || !status.updated_at) return false
  const updatedAt = new Date(status.updated_at).getTime()
  if (Number.isNaN(updatedAt)) return false
  return now - updatedAt > BACKFILL_STALL_THRESHOLD_MS
}

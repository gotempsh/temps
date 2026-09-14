// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { randomUUID } from 'node:crypto'
import type { SetupStep } from './provision.js'

// Per-attempt ID, not a machine identifier. No destination, email or raw errors.
export function setupTelemetry(enabled: boolean, version: string, send: typeof fetch = fetch) {
  const id = enabled ? randomUUID() : undefined
  const started = Date.now()
  const events: object[] = []
  return {
    record(step: SetupStep, status: 'started' | 'completed' | 'failed') {
      if (!enabled) return
      events.push({
        anonymous_id: id,
        event_type: 'cli_setup_step',
        properties: {
          step, status, method: 'ssh', cli_version: version,
          elapsed_bucket: Date.now() - started < 60_000 ? 'under_minute' : Date.now() - started < 300_000 ? 'under_five_minutes' : 'five_minutes_plus',
        },
      })
    },
    async flush() {
      if (!enabled || events.length === 0) return
      try {
        await send('https://telemetry.temps.sh/v1/events/batch', {
          method: 'POST', redirect: 'error',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ events: events.splice(0) }),
          signal: AbortSignal.timeout(2000),
        })
      } catch { /* Analytics never changes the setup outcome. */ }
    },
  }
}

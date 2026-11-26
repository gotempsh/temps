// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps alarms` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  acknowledgeAlarm,
  acknowledgeSystemAlarm,
  resolveAlarm,
  resolveSystemAlarm,
  silenceAlarm,
  silenceSystemAlarm,
  unsilenceAlarm,
  unsilenceSystemAlarm,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerAlarmsEndpoints(program: Command): void {
  const group = topGroup(program, 'alarms', "alarms")

  endpoint(group, acknowledgeAlarm, {
    name: "acknowledge-alarm",
    description: "Acknowledge a firing alarm (marks it as seen but not resolved)",
    path: { project_id: 'project', alarm_id: 'int' },
  })

  endpoint(group, resolveAlarm, {
    name: "resolve-alarm",
    description: "Resolve an alarm",
    path: { project_id: 'project', alarm_id: 'int' },
  })

  endpoint(group, unsilenceAlarm, {
    name: "unsilence",
    description: "Lift an alarm's silence before it expires, so the next breach of the same alert notifies again",
    path: { project_id: 'project', alarm_id: 'int' },
    confirm: "This will lift an alarm's silence before it expires, so the next breach of the same alert notifies again.",
  })

  endpoint(group, silenceAlarm, {
    name: "silence",
    description: "Silence an alarm (and future re-fires of the same type/scope) for a chosen duration, without permanently..",
    path: { project_id: 'project', alarm_id: 'int' },
    body: {
      duration_hours: { kind: 'int!', help: "How long to mute this alarm (and future re-fires of the same type/scope) for, in hours" },
    },
  })

  endpoint(group, acknowledgeSystemAlarm, {
    name: "system acknowledge",
    description: "Acknowledge a firing system alarm",
    path: { alarm_id: 'int' },
  })

  endpoint(group, resolveSystemAlarm, {
    name: "system resolve",
    description: "Resolve a system alarm",
    path: { alarm_id: 'int' },
  })

  endpoint(group, unsilenceSystemAlarm, {
    name: "system unsilence",
    description: "Lift a system alarm's silence before it expires",
    path: { alarm_id: 'int' },
    confirm: "This will lift a system alarm's silence before it expires.",
  })

  endpoint(group, silenceSystemAlarm, {
    name: "system silence",
    description: "Silence a system alarm (and future re-fires of the same type/scope) for a chosen duration, without..",
    path: { alarm_id: 'int' },
    body: {
      duration_hours: { kind: 'int!', help: "How long to mute this alarm (and future re-fires of the same type/scope) for, in hours" },
    },
  })
}

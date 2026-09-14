// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { updateMonitor } from './client'

/** Update a monitor without replacing it or discarding its uptime history. */
export async function updateMonitorPath(monitorId: number, checkPath: string) {
  const { data } = await updateMonitor({
    path: { monitor_id: monitorId },
    body: { check_path: checkPath },
    throwOnError: true,
  })
  return data
}

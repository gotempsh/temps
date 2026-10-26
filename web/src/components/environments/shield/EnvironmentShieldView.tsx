// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { ShieldEntriesPanel } from './ShieldEntriesPanel'
import { ShieldEventsPanel } from './ShieldEventsPanel'
import { ShieldLookupPanel } from './ShieldLookupPanel'
import { ShieldSummaryPanel } from './ShieldSummaryPanel'
import { ShieldTokensPanel } from './ShieldTokensPanel'

/**
 * The Shield tab of one environment (ADR 066 D11): its mode, what Shield
 * saw, recent events, an address lookup, bypass tokens and the project's
 * operator entries.
 */
export function EnvironmentShieldView({
  project,
  environment,
  onOpenSettings,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
  onOpenSettings?: () => void
}) {
  const path = { project_id: project.id, environment_id: environment.id }
  return (
    // Keyed so cursors, lookups and a freshly minted token never carry over
    // from one environment to the next.
    <div key={environment.id} className="w-full min-w-0 space-y-8">
      <ShieldSummaryPanel path={path} onOpenSettings={onOpenSettings} />
      <ShieldLookupPanel path={path} />
      <ShieldEventsPanel path={path} />
      <ShieldTokensPanel path={path} environmentName={environment.name} />
      <ShieldEntriesPanel projectId={project.id} />
    </div>
  )
}

export interface ShieldEnvironmentPath {
  project_id: number
  environment_id: number
}

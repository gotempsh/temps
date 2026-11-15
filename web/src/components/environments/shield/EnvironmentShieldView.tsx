// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { SHIELD_EVENTS_PARAMS } from '@/lib/environment-navigation'
import type { ShieldWindowValue } from '@/lib/shield'
import { useUrlState } from '@temps-sdk/ds'
import { ArrowRight } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { ShieldEventsList } from './ShieldEventsList'
import { ShieldEventsPanel } from './ShieldEventsPanel'
import { ShieldLookupPanel } from './ShieldLookupPanel'
import { ShieldGroup, ShieldSection } from './ShieldSection'
import {
  ShieldModeStatus,
  ShieldSources,
  ShieldVerdictStats,
  ShieldWindowSelect,
} from './ShieldSummary'
import { ShieldTokensPanel } from './ShieldTokensPanel'

/**
 * The Shield tab of one environment (ADR 066 D11), in three levels: its mode
 * first, then Activity (what Shield saw in a time window: counts, recent
 * events, sources), then Tools (address lookup, bypass tokens, entries).
 * Entries apply to more than one environment, so they are edited where they
 * apply: the service's Security settings and the platform's.
 * `?shield=events` shows every event instead of the overview.
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
  const url = useUrlState<(typeof SHIELD_EVENTS_PARAMS)[number]>()
  const [timeWindow, setTimeWindow] = useState<ShieldWindowValue>('24h')
  if (url.state.shield === 'events') {
    return (
      <div key={environment.id} className="w-full min-w-0">
        <ShieldEventsList
          path={path}
          onBack={() => url.clear([...SHIELD_EVENTS_PARAMS])}
        />
      </div>
    )
  }
  return (
    // Keyed so cursors, lookups and a freshly minted token never carry over
    // from one environment to the next.
    <div key={environment.id} className="w-full min-w-0 space-y-10">
      <ShieldModeStatus
        path={path}
        timeWindow={timeWindow}
        onOpenSettings={onOpenSettings}
      />
      <ShieldGroup
        id="shield-activity"
        title="Activity"
        description="What Shield decided for this environment's requests."
        actions={
          <ShieldWindowSelect value={timeWindow} onChange={setTimeWindow} />
        }
      >
        <ShieldVerdictStats path={path} timeWindow={timeWindow} />
        <ShieldEventsPanel
          path={path}
          onShowAll={() => url.patch({ shield: 'events' })}
        />
        <ShieldSources path={path} timeWindow={timeWindow} />
      </ShieldGroup>
      <ShieldGroup
        id="shield-tools"
        title="Tools"
        description="Check what Shield would decide, let trusted clients through, and manage entries."
      >
        <ShieldLookupPanel path={path} />
        <ShieldTokensPanel path={path} environmentName={environment.name} />
        <ShieldEntriesLinks projectSlug={project.slug} />
      </ShieldGroup>
    </div>
  )
}

function ShieldEntriesLinks({ projectSlug }: { projectSlug: string }) {
  const isAdmin = useIsInstanceAdmin()
  return (
    <ShieldSection
      id="shield-entries"
      title="Entries"
      description="Addresses Shield challenges or logs, and TLS fingerprints it allows, blocks or challenges. Entries apply to every environment of the service, so they are kept in its Security settings, next to IP access."
    >
      <div className="flex flex-wrap gap-2 p-4">
        <Button asChild size="sm" variant="outline">
          <Link
            to={`/projects/${projectSlug}/settings/security#shield-entries`}
          >
            Service entries
            <ArrowRight className="ml-2 h-4 w-4" />
          </Link>
        </Button>
        {isAdmin && (
          <Button asChild size="sm" variant="outline">
            <Link to="/settings/security#shield">
              Platform entries
              <ArrowRight className="ml-2 h-4 w-4" />
            </Link>
          </Button>
        )}
      </div>
    </ShieldSection>
  )
}

export interface ShieldEnvironmentPath {
  project_id: number
  environment_id: number
}

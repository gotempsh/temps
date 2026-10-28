// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { SHIELD_EVENTS_PARAMS } from '@/lib/environment-navigation'
import { useUrlState } from '@temps-sdk/ds'
import { ArrowRight } from 'lucide-react'
import { Link } from 'react-router'
import { ShieldEventsList } from './ShieldEventsList'
import { ShieldEventsPanel } from './ShieldEventsPanel'
import { ShieldLookupPanel } from './ShieldLookupPanel'
import { ShieldSection } from './ShieldSection'
import { ShieldSummaryPanel } from './ShieldSummaryPanel'
import { ShieldTokensPanel } from './ShieldTokensPanel'

/**
 * The Shield tab of one environment (ADR 066 D11): its mode, what Shield
 * saw, recent events, an address lookup and bypass tokens. Entries apply to
 * more than one environment, so they are edited where they apply: the
 * service's Security settings and the platform's. `?shield=events` shows
 * every event instead of the overview.
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
    <div key={environment.id} className="w-full min-w-0 space-y-8">
      <ShieldSummaryPanel path={path} onOpenSettings={onOpenSettings} />
      <ShieldLookupPanel path={path} />
      <ShieldEventsPanel
        path={path}
        onShowAll={() => url.patch({ shield: 'events' })}
      />
      <ShieldTokensPanel path={path} environmentName={environment.name} />
      <ShieldEntriesLinks projectSlug={project.slug} />
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Activity, Settings } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { formatAge } from '@/lib/server-monitoring'

/**
 * Shown on the Server page when the metrics store answered but holds no node
 * sample in the selected window, in place of five usage cards and charts that would each repeat
 * a bare "no samples" caption. It says what is missing (the sampler has not
 * written a point), when to expect one, and where collection is configured.
 */
export function NoNodeSamplesState({
  range,
  scrapeInterval,
  settingsPath,
}: {
  /** The selected window ("1h", "24h", ...). */
  range: string
  /** Seconds between samples, from the monitoring settings. */
  scrapeInterval: number
  settingsPath: string
}) {
  return (
    <div className="rounded-lg border bg-card text-card-foreground">
      <EmptyState
        size="compact"
        icon={Activity}
        title="No server samples in this window"
        description={`No CPU, memory, disk or network sample was recorded for this host in the last ${range}. The sampler runs inside temps serve and writes one every ${formatAge(scrapeInterval)}, so a fresh install fills in shortly after it starts. If none arrive, check that temps serve is running and review the collection settings.`}
        action={
          <Button asChild variant="outline" size="sm">
            <Link to={settingsPath}>
              <Settings className="mr-2 size-4" />
              Metrics monitoring settings
            </Link>
          </Button>
        }
      />
    </div>
  )
}

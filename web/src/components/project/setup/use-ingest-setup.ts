// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import type { IngestSignalSetup, ProjectIngestSetup } from '@/api/client'
import { getProjectIngestSetupOptions } from '@/api/client/@tanstack/react-query.gen'
import type { IngestSignalName } from '@/lib/plain-http-setup'

/** The ingest setup of a project, shared by every setup surface. */
export function useIngestSetup(projectId: number, environmentId?: number) {
  return useQuery({
    ...getProjectIngestSetupOptions({
      path: { project_id: projectId },
      query: environmentId ? { environment_id: environmentId } : undefined,
    }),
    enabled: !!projectId,
    staleTime: 60_000,
  })
}

export function findSignal(
  setup: ProjectIngestSetup | undefined,
  signal: IngestSignalName
): IngestSignalSetup | undefined {
  return setup?.signals.find((s) => s.signal === signal)
}

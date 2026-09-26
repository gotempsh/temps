// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { useSearchParams } from 'react-router'
import { BuildSettings } from './GitSettings'
import { DeployDefaultsCard } from './DeployDefaultsCard'
import { DeploymentSourceCard } from './DeploymentSourceCard'
import { EnvironmentPortOverrideCard } from './EnvironmentPortOverrideCard'
import { ImageRetentionCard } from './ImageRetentionCard'
import { PreviewEnvironmentsCard } from './PreviewEnvironmentsCard'
import { ServiceTemplateRuntimeCard } from './ServiceTemplateRuntimeCard'

const TABS = ['source', 'build', 'deploy', 'previews'] as const
type TabValue = (typeof TABS)[number]

function isTab(value: string | null): value is TabValue {
  return value !== null && (TABS as readonly string[]).includes(value)
}

/**
 * Everything about how a project turns into a running deployment, in pipeline
 * order: where the code comes from, how it is built, how it is rolled out, and
 * how throwaway branch environments behave.
 *
 * These cards used to be split between this page and General, which meant the
 * page that owns `preset`/`directory` said nothing about the deployment source
 * that can rewrite them. Observability toggles deliberately stay on General —
 * they describe what a deployment reports, not how it ships.
 *
 * The flat Settings sidebar selects the page. Existing tab query parameters
 * remain supported so saved links keep opening the same configuration.
 */
export function BuildDeploySettings({
  project,
  refetch,
  section,
}: {
  project: ProjectResponse
  refetch: () => void
  section?: TabValue
}) {
  const [searchParams] = useSearchParams()
  const requested = searchParams.get('tab')
  const active: TabValue = section ?? (isTab(requested) ? requested : 'source')

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-xl font-semibold text-balance">
          {
            {
              source: 'Source',
              build: 'Build',
              deploy: 'Deployment',
              previews: 'Preview environments',
            }[active]
          }
        </h2>
        <p className="max-w-[72ch] text-pretty text-base/7 text-muted-foreground sm:text-sm/6">
          Configure how Temps turns your source into a running application.
        </p>
      </div>

      {active === 'source' && (
        <div className="space-y-6">
          <DeploymentSourceCard project={project} refetch={refetch} />
        </div>
      )}

      {active === 'build' && (
        <div className="space-y-6">
          {project.project_type === 'service' ? (
            <ServiceTemplateRuntimeCard project={project} refetch={refetch} />
          ) : (
            <BuildSettings project={project} refetch={refetch} embedded />
          )}
        </div>
      )}

      {active === 'deploy' && (
        <div className="space-y-6">
          <DeployDefaultsCard project={project} refetch={refetch} />
          <EnvironmentPortOverrideCard project={project} />
          <ImageRetentionCard project={project} refetch={refetch} />
        </div>
      )}

      {active === 'previews' && (
        <div className="space-y-6">
          <PreviewEnvironmentsCard project={project} refetch={refetch} />
        </div>
      )}
    </div>
  )
}

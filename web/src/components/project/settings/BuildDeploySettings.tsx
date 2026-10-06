// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { BuildSettings } from './GitSettings'
import { DeployDefaultsCard } from './DeployDefaultsCard'
import { DeploymentSourceCard } from './DeploymentSourceCard'
import { EnvironmentPortOverrideCard } from './EnvironmentPortOverrideCard'
import { ImageRetentionCard } from './ImageRetentionCard'
import { PreviewEnvironmentsCard } from './PreviewEnvironmentsCard'
import { ServiceTemplateRuntimeCard } from './ServiceTemplateRuntimeCard'

type BuildDeploySection = 'source' | 'build' | 'deploy' | 'previews'

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
 * Each part renders inside its own titled section of the Build & deploy page,
 * which owns the heading. Links to the old `?tab=` page are redirected to the
 * matching section (`legacyProjectRouteTarget`).
 */
export function BuildDeploySettings({
  project,
  refetch,
  section,
}: {
  project: ProjectResponse
  refetch: () => void
  section: BuildDeploySection
}) {
  const active = section

  return (
    <div className="space-y-6">
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { PageHeader } from '@temps-sdk/ds'
import type { ProjectResponse } from '@/api/client'
import { SourceMaps } from '@/components/error-tracking/SourceMaps'
import { DeploymentTokensSettings } from '@/components/project/settings/DeploymentTokensSettings'

// Mixed setup, grouped artifacts and upload workflow: compose the shared header
// around existing native management panels, without duplicating their services.
export function SourceMapArtifacts({ project }: { project: ProjectResponse }) {
  return (
    <div className="space-y-6">
      <PageHeader
        title="Source maps"
        description="Resolve minified JavaScript with Debug IDs or release source maps. Artifacts are stored and managed by Temps."
      />
      <SourceMaps project={project} />
    </div>
  )
}
export function SourceMapCredentials({
  project,
}: {
  project: ProjectResponse
}) {
  return (
    <div className="space-y-6">
      <PageHeader
        title="Source map build credentials"
        description="Create a project deployment token with full access for sentry-cli. Keep build tokens in CI secrets; browser bundles use only the public DSN."
      />
      <DeploymentTokensSettings project={project} />
    </div>
  )
}

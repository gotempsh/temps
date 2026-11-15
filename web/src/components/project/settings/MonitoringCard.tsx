// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import type { DeploymentConfig, ProjectResponse } from '@/api/client'
import { updateProjectDeploymentConfigMutation } from '@/api/client/@tanstack/react-query.gen'
import { SettingToggleRow } from '@/components/settings/SettingToggleRow'

type MonitoringField = 'performanceMetricsEnabled' | 'sessionRecordingEnabled'

/**
 * Observability toggles that ride on the deployment config, as switch rows
 * that save when flipped.
 *
 * These live here rather than under Build & Deploy because they describe what
 * a running deployment reports, not how it is built or shipped. They share the
 * deployment-config endpoint with [DeployDefaultsCard], but each sends only
 * its own field and the service applies `if let Some(..)` per field, so
 * neither resets the other.
 */
export function MonitoringToggles({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: { errorTitle: 'Failed to update monitoring settings' },
    onSuccess: () => refetch(),
  })
  const saved = (field: MonitoringField) =>
    project.deployment_config?.[field] ?? false
  // Show the requested value while it saves.
  const value = (field: MonitoringField) => {
    const pending = updateDeploymentConfig.isPending
      ? (
          updateDeploymentConfig.variables?.body as DeploymentConfig | undefined
        )?.[field]
      : undefined
    return pending ?? saved(field)
  }
  const toggle = (field: MonitoringField) => (checked: boolean) =>
    updateDeploymentConfig.mutate({
      path: { project_id: project.id },
      body: { [field]: checked },
    })

  return (
    <>
      <SettingToggleRow
        id="performance-metrics"
        label="Performance metrics"
        description="Collect and display performance metrics for your deployments."
        checked={value('performanceMetricsEnabled')}
        onCheckedChange={toggle('performanceMetricsEnabled')}
        disabled={updateDeploymentConfig.isPending}
      />
      <SettingToggleRow
        id="session-recording"
        label="Session recording"
        description="Record user sessions for debugging and analytics."
        checked={value('sessionRecordingEnabled')}
        onCheckedChange={toggle('sessionRecordingEnabled')}
        disabled={updateDeploymentConfig.isPending}
      />
    </>
  )
}

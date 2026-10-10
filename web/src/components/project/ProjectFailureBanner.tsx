// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  DeploymentResponse,
  EnvironmentResponse,
  ProjectResponse,
} from '@/api/client'
import { RecoveryActionDialog } from '@/components/monitoring/RecoveryActionDialog'
import { Button } from '@/components/ui/button'
import { useProjectFailureState } from '@/hooks/useProjectFailureState'
import { failureSettingsLink } from '@/lib/deployment-failure-guidance'
import { deploymentFailureSummary } from '@/lib/deployment-failure-summary'
import { deploymentRedeployPlan } from '@/lib/deployment-source-summary'
import type { ContainerHealthIssue } from '@/lib/project-failure-state'
import type { RecoveryAction } from '@/lib/recovery-actions'
import {
  AlertTriangle,
  ArrowRight,
  RotateCcw,
  RotateCw,
  Settings,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'

interface ProjectFailureBannerProps {
  project: ProjectResponse
  lastDeployment?: DeploymentResponse
}

/**
 * The overview's Failed / Degraded banner: why the project is unhealthy and
 * the one-click fixes (redeploy, roll back to the last successful
 * deployment, restart a container). Renders nothing while all is well; the
 * confirmation dialog stays mounted either way.
 */
export function ProjectFailureBanner({
  project,
  lastDeployment,
}: ProjectFailureBannerProps) {
  const [pendingAction, setPendingAction] = useState<RecoveryAction | null>(
    null
  )
  const {
    failedDeployment,
    rollbackTarget,
    containerIssues,
    healthEnvironment,
    environmentsQuery,
  } = useProjectFailureState(project.id, lastDeployment)

  const failedEnvironment = failedDeployment
    ? environmentsQuery.data?.find(
        (environment) => environment.id === failedDeployment.environment_id
      )
    : undefined

  return (
    <>
      {failedDeployment && (
        <FailedDeploymentNotice
          project={project}
          deployment={failedDeployment}
          environment={failedEnvironment}
          rollbackTarget={rollbackTarget}
          onAction={setPendingAction}
        />
      )}
      {healthEnvironment && containerIssues.length > 0 && (
        <DegradedContainersNotice
          project={project}
          environment={healthEnvironment}
          issues={containerIssues}
          onAction={setPendingAction}
        />
      )}
      <RecoveryActionDialog
        action={pendingAction}
        onClose={() => setPendingAction(null)}
      />
    </>
  )
}

function FailedDeploymentNotice({
  project,
  deployment,
  environment,
  rollbackTarget,
  onAction,
}: {
  project: ProjectResponse
  deployment: DeploymentResponse
  environment: EnvironmentResponse | undefined
  rollbackTarget: DeploymentResponse | undefined
  onAction: (action: RecoveryAction) => void
}) {
  const failure = deployment.failure
  const reason = deployment.cancelled_reason
    ? deploymentFailureSummary(deployment.cancelled_reason).summary
    : null
  const settingsLink = failure
    ? failureSettingsLink(failure.settings_section, project.slug)
    : null
  const canRedeploy =
    deploymentRedeployPlan(deployment, project.source_type).kind !==
    'unsupported'
  const liveId = environment?.current_deployment_id
  const environmentName = deployment.environment?.name ?? environment?.name
  const liveNote =
    liveId != null
      ? `#${liveId} is still serving traffic${environmentName ? ` in ${environmentName}` : ''}.`
      : `Nothing is live${environmentName ? ` in ${environmentName}` : ''}.`

  return (
    <div
      role="alert"
      data-testid="project-failure-banner"
      className="mb-4 flex items-start gap-2.5 rounded-lg border border-destructive/30 bg-destructive/5 p-4"
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
      <div className="min-w-0 flex-1 space-y-2">
        <p className="text-sm font-medium text-destructive">
          {`Latest deployment #${deployment.id} failed`}
          {failure ? `: ${failure.title}` : ''}
        </p>
        {failure ? (
          <p className="text-sm">
            <span className="font-medium">How to fix: </span>
            {failure.remediation}
          </p>
        ) : reason ? (
          <p className="whitespace-pre-wrap break-words text-sm text-destructive/80">
            {reason}
          </p>
        ) : null}
        <p className="text-xs text-muted-foreground">{liveNote}</p>
        <div className="flex flex-wrap items-center gap-2 pt-1">
          {canRedeploy && (
            <Button
              size="sm"
              variant="outline"
              onClick={() =>
                onAction({
                  kind: 'redeploy',
                  projectId: project.id,
                  sourceType: project.source_type,
                  deploymentId: deployment.id,
                })
              }
            >
              <RotateCw className="size-3.5" />
              Redeploy
            </Button>
          )}
          {rollbackTarget && (
            <Button
              size="sm"
              variant="outline"
              onClick={() =>
                onAction({
                  kind: 'rollback',
                  projectId: project.id,
                  targetDeploymentId: rollbackTarget.id,
                  targetIsLive: rollbackTarget.id === liveId,
                })
              }
            >
              <RotateCcw className="size-3.5" />
              {`Roll back to #${rollbackTarget.id} (last successful)`}
            </Button>
          )}
          {settingsLink && (
            <Button size="sm" variant="outline" asChild>
              <Link to={settingsLink.href}>
                <Settings className="size-3.5" />
                {`Open ${settingsLink.label}`}
              </Link>
            </Button>
          )}
          <Button size="sm" variant="ghost" asChild>
            <Link to={`/projects/${project.slug}/deployments/${deployment.id}`}>
              View deployment
              <ArrowRight className="size-3.5" />
            </Link>
          </Button>
        </div>
      </div>
    </div>
  )
}

function DegradedContainersNotice({
  project,
  environment,
  issues,
  onAction,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
  issues: ContainerHealthIssue[]
  onAction: (action: RecoveryAction) => void
}) {
  const noun = issues.length === 1 ? 'container is' : 'containers are'
  return (
    <div
      role="alert"
      data-testid="project-degraded-banner"
      className="mb-4 flex items-start gap-2.5 rounded-lg border border-amber-500/40 bg-amber-500/5 p-4"
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-700 dark:text-amber-400" />
      <div className="min-w-0 flex-1 space-y-2">
        <p className="text-sm font-medium text-amber-700 dark:text-amber-400">
          {`${issues.length} ${noun} down or restarting in ${environment.name}`}
        </p>
        <ul className="divide-y divide-amber-500/20">
          {issues.map((issue) => (
            <li
              key={issue.containerId}
              className="flex flex-col gap-2 py-2 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between"
            >
              <div className="min-w-0">
                <p className="truncate text-sm font-medium">
                  {issue.containerName}
                </p>
                <p className="truncate text-xs text-muted-foreground">
                  {issue.detail}
                </p>
              </div>
              <div className="flex shrink-0 gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    onAction({
                      kind: 'restart_container',
                      projectId: project.id,
                      environmentId: environment.id,
                      containerId: issue.containerId,
                      containerName: issue.containerName,
                    })
                  }
                >
                  <RotateCw className="size-3.5" />
                  Restart
                </Button>
                <Button size="sm" variant="ghost" asChild>
                  <Link
                    to={`/projects/${project.slug}/environments/containers/${encodeURIComponent(issue.containerId)}?env=${environment.id}`}
                  >
                    Logs
                    <ArrowRight className="size-3.5" />
                  </Link>
                </Button>
              </div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}

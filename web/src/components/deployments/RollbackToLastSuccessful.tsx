// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'
import { RecoveryActionDialog } from '@/components/monitoring/RecoveryActionDialog'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useLastSuccessfulDeployment } from '@/hooks/useLastSuccessfulDeployment'
import type { RecoveryAction } from '@/lib/recovery-actions'
import { RotateCcw } from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router'

/**
 * "Roll back to #N (last successful)" for a failed deployment's banner: finds
 * the newest earlier deployment in the same environment that can be rolled
 * back to and offers it in one click (with a confirmation). Renders nothing
 * when the environment never had a successful deployment.
 */
export function RollbackToLastSuccessful({
  deployment,
  projectSlug,
}: {
  deployment: DeploymentResponse
  projectSlug: string
}) {
  const navigate = useNavigate()
  const [pendingAction, setPendingAction] = useState<RecoveryAction | null>(
    null
  )
  const targetQuery = useLastSuccessfulDeployment(deployment)
  const target = targetQuery.data

  return (
    <>
      {targetQuery.isPending ? (
        <Skeleton className="h-7 w-56" />
      ) : target ? (
        <Button
          variant="outline"
          size="sm"
          className="h-7"
          data-testid="rollback-to-last-successful"
          onClick={() =>
            setPendingAction({
              kind: 'rollback',
              projectId: deployment.project_id,
              targetDeploymentId: target.id,
              targetIsLive: target.is_current,
            })
          }
        >
          <RotateCcw className="mr-1.5 h-3 w-3" />
          {`Roll back to #${target.id} (last successful)`}
        </Button>
      ) : null}
      <RecoveryActionDialog
        action={pendingAction}
        onClose={() => setPendingAction(null)}
        onDone={() =>
          navigate(`/projects/${projectSlug}/deployments?autoRefresh=true`)
        }
      />
    </>
  )
}

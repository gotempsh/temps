// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useNodeCapability } from '@/hooks/useNodeCapability'
import {
  describeDockerSocket,
  HOST_DOCKER_ACCESS_EXPLANATION,
  hostDockerAccessPlacement,
  type HostDockerAccessPlacement,
} from '@/lib/docker-socket'
import { canAddWorkerNode, sameOriginSetupPath } from '@/lib/worker-nodes'
import { Plug, ShieldAlert } from 'lucide-react'
import { Link } from 'react-router'

/**
 * Where Build & deploy shows the ADR-045 host Docker socket grant (see
 * `docs/adr/045-host-docker-socket-grant.md`): leading the page when the
 * project holds it, in the collapsed "Advanced" section when an operator
 * could grant it, nowhere otherwise.
 */
export function useHostDockerAccess(project: ProjectResponse): {
  placement: HostDockerAccessPlacement
  canManageNodes: boolean
} {
  const { data: nodeCapability } = useNodeCapability()
  const canManageNodes = canAddWorkerNode(nodeCapability)
  return {
    placement: hostDockerAccessPlacement(project.docker_socket, canManageNodes),
    canManageNodes,
  }
}

function ViewHostsButton({ project }: { project: ProjectResponse }) {
  const setupPath = sameOriginSetupPath(
    project.docker_socket?.setup_path ?? undefined
  )
  return (
    <Button asChild size="sm" variant="outline">
      <Link to={setupPath}>View hosts</Link>
    </Button>
  )
}

/**
 * A granted project is root-equivalent on every host that grants it. Anyone
 * changing how it builds and deploys should see that before anything else.
 * The link to the hosts page only renders for a viewer who can open it.
 */
export function HostDockerAccessAlert({
  project,
  canManageNodes,
}: {
  project: ProjectResponse
  canManageNodes: boolean
}) {
  const description = describeDockerSocket(project.docker_socket)
  return (
    <Alert>
      <ShieldAlert className="h-4 w-4" />
      <AlertTitle>{description.label}</AlertTitle>
      <AlertDescription>
        <p>{description.detail}</p>
        <p className="mt-2">{HOST_DOCKER_ACCESS_EXPLANATION}</p>
        {canManageNodes && (
          <div className="mt-3">
            <ViewHostsButton project={project} />
          </div>
        )}
      </AlertDescription>
    </Alert>
  )
}

/**
 * Onboarding for a project without the grant, shown to operators only.
 *
 * The grant is host policy — an environment variable on the machine that runs
 * the container — so there is nothing to click here and deliberately no write
 * path: an API that could set it would be one step from host root. What the
 * operator gets instead is what the capability does, that this project does
 * not have it, the exact variable to set, and the page listing the hosts they
 * could set it on.
 */
export function HostDockerAccessOnboarding({
  project,
}: {
  project: ProjectResponse
}) {
  const description = describeDockerSocket(project.docker_socket)
  return (
    <div className="space-y-2 text-sm">
      <p className="flex items-center gap-2 font-medium">
        <Plug className="size-4 text-muted-foreground" aria-hidden="true" />
        Not granted
      </p>
      <p className="text-muted-foreground">{HOST_DOCKER_ACCESS_EXPLANATION}</p>
      <p className="text-muted-foreground">{description.detail}</p>
      <div className="pt-1">
        <ViewHostsButton project={project} />
      </div>
    </div>
  )
}

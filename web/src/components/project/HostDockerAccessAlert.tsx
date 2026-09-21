// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useNodeCapability } from '@/hooks/useNodeCapability'
import {
  describeDockerSocket,
  HOST_DOCKER_ACCESS_EXPLANATION,
  shouldShowDockerSocketOnboarding,
} from '@/lib/docker-socket'
import { canAddWorkerNode, sameOriginSetupPath } from '@/lib/worker-nodes'
import { Plug } from 'lucide-react'
import { Link } from 'react-router'

/**
 * Onboarding for the ADR-045 host Docker socket grant (see
 * `docs/adr/045-host-docker-socket-grant.md`).
 *
 * The grant is host policy — an environment variable on the machine that runs
 * the container — so there is nothing to click here and deliberately no write
 * path: an API that could set it would be one step from host root. What the
 * operator gets instead is the thing they cannot get from a silent absence:
 * what the capability does, that this project does not have it, the exact
 * variable to set, and the page listing the hosts they could set it on.
 *
 * Renders nothing when the project already holds the grant (the header badge
 * says so), when the response did not compute the capability, or for a viewer
 * who cannot manage nodes — the setup page needs Settings permissions, so
 * linking them there would be a dead end.
 */
export function HostDockerAccessAlert({
  project,
}: {
  project: ProjectResponse
}) {
  const { data: nodeCapability } = useNodeCapability()

  const capability = project.docker_socket
  const description = describeDockerSocket(capability)

  if (
    !shouldShowDockerSocketOnboarding(
      capability,
      canAddWorkerNode(nodeCapability)
    )
  ) {
    return null
  }

  const setupPath = sameOriginSetupPath(capability?.setup_path ?? undefined)

  return (
    <Alert>
      <Plug className="h-4 w-4" />
      <AlertTitle>{description.label}</AlertTitle>
      <AlertDescription>
        <p>{HOST_DOCKER_ACCESS_EXPLANATION}</p>
        <p className="mt-2">{description.detail}</p>
        <div className="mt-3">
          <Button asChild size="sm" variant="outline">
            <Link to={setupPath}>View hosts</Link>
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  )
}

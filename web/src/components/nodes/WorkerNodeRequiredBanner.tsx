// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useNodeCapability } from '@/hooks/useNodeCapability'
import {
  shouldShowWorkerNodeBanner,
  WORKER_NODES_DOCS_URL,
  WORKER_NODES_URL,
  WORKER_NODE_REQUIRED_MESSAGE,
  WORKER_NODE_REQUIRED_TITLE,
} from '@/lib/worker-nodes'
import { ExternalLink, Network } from 'lucide-react'
import { Link } from 'react-router'

/**
 * Compact banner telling the operator that nothing can run here yet.
 *
 * Deliberately not a full-page takeover: the page behind it still lists what
 * already exists, and the platform is one `temps join` away from working.
 */
export function WorkerNodeRequiredAlert({
  reason,
  setupPath = WORKER_NODES_URL,
  showSetupAction = true,
}: {
  reason?: string | null
  setupPath?: string
  /**
   * Set to false on the Nodes page itself, where the "Add worker node" button
   * would link to the page the user is already reading.
   */
  showSetupAction?: boolean
}) {
  return (
    <Alert variant="warning">
      <Network className="h-4 w-4" />
      <AlertTitle>{WORKER_NODE_REQUIRED_TITLE}</AlertTitle>
      <AlertDescription>
        <p>{reason || WORKER_NODE_REQUIRED_MESSAGE}</p>
        <div className="mt-3 flex flex-col gap-2 sm:flex-row">
          {showSetupAction && (
            <Button asChild size="sm" className="min-h-11 sm:min-h-9">
              <Link to={setupPath}>Add worker node</Link>
            </Button>
          )}
          <Button
            asChild
            size="sm"
            variant="outline"
            className="min-h-11 sm:min-h-9"
          >
            <a
              href={WORKER_NODES_DOCS_URL}
              target="_blank"
              rel="noopener noreferrer"
            >
              Learn how
              <ExternalLink className="h-3 w-3" />
            </a>
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  )
}

/**
 * Self-fetching variant for pages that let a user create a service or deploy.
 * Renders nothing while the capability is unknown or something can already
 * run the work — see `shouldShowWorkerNodeBanner`.
 */
export function WorkerNodeRequiredBanner() {
  const { data: capability } = useNodeCapability()

  if (!shouldShowWorkerNodeBanner(capability)) return null

  return (
    <WorkerNodeRequiredAlert
      reason={capability?.reason}
      setupPath={capability?.setup_path || WORKER_NODES_URL}
    />
  )
}

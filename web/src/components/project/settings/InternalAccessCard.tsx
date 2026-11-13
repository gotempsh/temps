// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Internal access (ADR 101, ADR 105): which other projects, and which of the
 * user's sandboxes, may call this project's `*.temps.local` addresses
 * through the internal proxy.
 *
 * By default only the project's own environments can call each other; every
 * other caller gets a 403. The card lists every other project and every one
 * of the user's sandboxes, each allowed in one click: all environments of
 * the caller may call this whole project, or the environment this card is
 * for. A narrower scope and a reason sit behind each row's "Limit…". The
 * project card manages every grant; an environment's card manages the grants
 * on that environment and shows the project-wide ones as inherited.
 */
import { useQuery } from '@tanstack/react-query'
import { Waypoints } from 'lucide-react'
import { SettingsSection } from '@/components/ui/settings-section'
import { InternalAccessProjectCallers } from './InternalAccessProjectCallers'
import { InternalAccessSandboxGrants } from './InternalAccessSandboxGrants'
import { useInternalAccessWriteBlock } from './internal-access-hooks'
import {
  projectGrantsOptions,
  sandboxGrantsOptions,
} from './internal-access-queries'
import {
  INTERNAL_ACCESS_SECTION_ID,
  exampleAddress,
  sandboxCurl,
} from './internal-access'

interface InternalAccessCardProps {
  projectId: number
  projectName: string
  projectSlug: string
  /** Set to manage the grants on one environment instead of the project. */
  environmentId?: number
  environmentName?: string
  environmentSlug?: string
}

export function InternalAccessCard({
  projectId,
  projectName,
  projectSlug,
  environmentId,
  environmentName,
  environmentSlug,
}: InternalAccessCardProps) {
  const isEnvironment = environmentId !== undefined
  const blockReason = useInternalAccessWriteBlock()
  // The lists ask the same; React Query shares the answers.
  const projectGrants = useQuery(projectGrantsOptions(projectId, environmentId))
  const sandboxGrants = useQuery(sandboxGrantsOptions(projectId, environmentId))
  const noGrants =
    projectGrants.data?.total === 0 && sandboxGrants.data?.total === 0
  const targetName = isEnvironment
    ? `${projectName} ${environmentName ?? 'this environment'}`
    : projectName

  return (
    <SettingsSection
      id={isEnvironment ? undefined : INTERNAL_ACCESS_SECTION_ID}
      title={
        isEnvironment
          ? 'Internal access to this environment'
          : 'Internal access'
      }
      description={
        isEnvironment
          ? `Other projects and sandboxes that may call ${exampleAddress(projectSlug, environmentSlug)}.`
          : `Other projects and sandboxes that may call this project's *.temps.local addresses.`
      }
      icon={Waypoints}
    >
      <div className="space-y-6">
        {noGrants ? (
          <p className="rounded-md border border-dashed p-3 text-sm text-muted-foreground [overflow-wrap:anywhere]">
            Only {projectName}&apos;s own environments can call it: a sandbox
            running{' '}
            <code className="text-xs">
              {sandboxCurl(projectSlug, environmentSlug)}
            </code>{' '}
            gets a 403. Allow a project or sandbox below.
          </p>
        ) : null}
        {blockReason ? (
          <p className="text-sm text-muted-foreground">{blockReason}</p>
        ) : null}
        <InternalAccessProjectCallers
          projectId={projectId}
          targetName={targetName}
          environmentId={environmentId}
          blockReason={blockReason}
        />
        <InternalAccessSandboxGrants
          projectId={projectId}
          targetName={targetName}
          environmentId={environmentId}
          blockReason={blockReason}
        />
      </div>
    </SettingsSection>
  )
}

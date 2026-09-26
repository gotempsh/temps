// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TeamRole } from '@/api/client/types.gen'

/**
 * What each role actually restricts today.
 *
 * Deliberately not phrased as "read-only" / "full control". Project roles
 * currently narrow a specific set of actions — deploying, environments and
 * environment variables, project settings, custom domains, and deleting
 * the project. Actions outside that set (backups, storage, sandboxes,
 * status pages, error tracking, and so on) are not yet narrowed by the
 * project role and still follow the member's instance-wide role.
 *
 * Describing a role as more restrictive than it is enforced to be is worse
 * than describing nothing, because an operator grants on the strength of
 * the label: "read-only" reads as a safe default for a contractor, and it
 * is not one yet. See ROLE_ENFORCEMENT_NOTE, which is rendered wherever a
 * role is chosen.
 */
export const ROLE_DESCRIPTIONS: Record<TeamRole, string> = {
  owner:
    'Deploy, manage env vars, settings and domains, and delete the project',
  admin:
    'Deploy, manage env vars, settings and domains. Cannot delete the project',
  deployer: 'Deploy and manage env vars. Cannot change settings or domains',
  viewer: 'Cannot deploy, or change env vars, settings or domains',
}

/**
 * Shown next to every role picker. The honest scope of what a role gates,
 * so nobody grants `viewer` expecting a read-only contractor account.
 */
export const ROLE_ENFORCEMENT_NOTE =
  'Roles currently restrict deployments, environment variables, project ' +
  'settings, domains and project deletion. Other actions still follow the ' +
  "member's instance-wide role."

export interface AddMemberDialogProps {
  teamId: number
  existingUserIds: number[]
  open: boolean
  onOpenChange: (open: boolean) => void
}

export interface EditTeamDialogProps {
  teamId: number
  name: string
  description: string | null | undefined
  open: boolean
  onOpenChange: (open: boolean) => void
}

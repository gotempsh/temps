// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Project access — which teams can reach this project.
 *
 * This is the screen where a project stops being open to everyone. With no
 * grants, any user holding the relevant instance permission can reach it;
 * the first grant restricts it to the listed teams (plus instance admins,
 * who are never scoped by team membership). The banner states which of the
 * two states the project is currently in, because that distinction is the
 * whole feature and is invisible otherwise.
 */
import { ProjectResponse } from '@/api/client'
import {
  grantProjectAccessMutation,
  listProjectAccessOptions,
  listProjectAccessQueryKey,
  listTeamsOptions,
  revokeProjectAccessMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectAccessResponse, TeamRole } from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Callout, DataTable, PageState } from '@temps-sdk/ds'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

import {
  ROLE_DESCRIPTIONS,
  ROLE_ENFORCEMENT_NOTE,
  RoleSelect,
} from '@/pages/TeamDetail'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Globe, Lock, Plus, Trash2, Users } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

interface ProjectAccessSettingsProps {
  project: ProjectResponse
}

export function ProjectAccessSettings({ project }: ProjectAccessSettingsProps) {
  const queryClient = useQueryClient()
  const [grantOpen, setGrantOpen] = useState(false)
  const [teamId, setTeamId] = useState<string>('')
  const [role, setRole] = useState<TeamRole>('viewer')
  const [grantToRevoke, setGrantToRevoke] =
    useState<ProjectAccessResponse | null>(null)

  const accessQueryKey = listProjectAccessQueryKey({
    path: { project_id: project.id },
  })

  const {
    data: grants,
    isLoading,
    isError,
    error,
    refetch,
  } = useQuery(listProjectAccessOptions({ path: { project_id: project.id } }))

  const {
    data: teamsData,
    isLoading: teamsLoading,
    isError: teamsFailed,
    refetch: retryTeams,
  } = useQuery(listTeamsOptions({ query: { page: 1, page_size: 100 } }))

  const teams = teamsData?.teams ?? []
  const grantList = grants ?? []

  // Regranting an existing team is an upsert server-side, but offering it in
  // the "add" picker reads as a bug — the row is already in the table.
  const availableTeams = useMemo(
    () => teams.filter((t) => !grantList.some((g) => g.team_id === t.id)),
    [teams, grantList]
  )

  const teamName = (id: number) =>
    teams.find((t) => t.id === id)?.name ?? `Team ${id}`

  const grantMutation = useMutation({
    ...grantProjectAccessMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: accessQueryKey })
      toast.success('Team access granted')
      setGrantOpen(false)
      setTeamId('')
      setRole('viewer')
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to grant access')
    },
  })

  const revokeMutation = useMutation({
    ...revokeProjectAccessMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: accessQueryKey })
      toast.success('Team access revoked')
      setGrantToRevoke(null)
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to revoke access')
    },
  })

  const isRestricted = grantList.length > 0

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-xl font-semibold">Access</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Control which teams can reach this project.
          </p>
        </div>
        <Button
          size="sm"
          onClick={() => setGrantOpen(true)}
          disabled={
            isLoading ||
            isError ||
            teamsLoading ||
            teamsFailed ||
            availableTeams.length === 0
          }
        >
          <Plus className="mr-2 h-4 w-4" />
          Grant access
        </Button>
      </div>

      {!isLoading &&
        !isError &&
        !teamsLoading &&
        !teamsFailed &&
        teams.length > 0 &&
        availableTeams.length === 0 && (
          <p className="text-sm text-muted-foreground">
            Every team in the current team list already has access.
          </p>
        )}

      {!isLoading && !isError && (
        <Alert>
          {isRestricted ? (
            <Lock className="h-4 w-4" />
          ) : (
            <Globe className="h-4 w-4" />
          )}
          <AlertTitle>
            {isRestricted
              ? 'Restricted to the teams below'
              : 'Open to everyone'}
          </AlertTitle>
          <AlertDescription>
            {isRestricted
              ? 'Only members of these teams — and instance administrators — can see or open this project. Revoking the last grant makes it open again.'
              : 'Any user with the relevant instance permission can see and open this project. Granting a team access is what restricts it.'}
          </AlertDescription>
        </Alert>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Users className="h-4 w-4" />
            Teams with access
          </CardTitle>
          <CardDescription>
            A member's permissions here are the narrower of their role in the
            team and the role the team holds on this project.{' '}
            {ROLE_ENFORCEMENT_NOTE}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-4">
            {teamsFailed && (
              <Callout tone="warning" title="Could not load teams">
                Existing grants are shown by team ID when names are unavailable.
                Retry to grant access to another team.
                <Button
                  variant="link"
                  size="sm"
                  onClick={() => void retryTeams()}
                >
                  Retry teams
                </Button>
              </Callout>
            )}
            {isError && grants !== undefined && (
              <Callout tone="error" title="Could not refresh access grants">
                Showing the last loaded grants.{' '}
                <Button variant="link" size="sm" onClick={() => void refetch()}>
                  Retry
                </Button>
              </Callout>
            )}
            {isError && grants === undefined ? (
              <PageState
                variant="failed"
                size="compact"
                icon={Users}
                title="Could not load access grants"
                description={
                  error instanceof Error
                    ? error.message
                    : 'The access API did not respond.'
                }
                action={<Button onClick={() => void refetch()}>Retry</Button>}
              />
            ) : !isLoading &&
              !teamsLoading &&
              !teamsFailed &&
              teams.length === 0 &&
              grantList.length === 0 ? (
              <PageState
                variant="not-set-up"
                size="compact"
                icon={Users}
                title="Create a team to restrict access"
                requirement="No teams exist yet."
                example="Give your operations team access to this project."
                settingsHref="/settings/teams"
                settingsLabel="Go to Teams"
              />
            ) : !isLoading && grantList.length === 0 && !teamsLoading ? (
              <PageState
                variant="empty"
                size="compact"
                icon={Globe}
                title="No team restrictions"
                description="Grant a team access to restrict this project to its members."
                action={
                  !teamsFailed && (
                    <Button onClick={() => setGrantOpen(true)}>
                      <Plus className="size-4 mr-2" />
                      Grant access
                    </Button>
                  )
                }
              />
            ) : (
              <DataTable
                aria-label="Teams with project access"
                rows={grantList}
                rowKey={(grant) => grant.id}
                isLoading={
                  isLoading || (teamsLoading && grantList.length === 0)
                }
                columns={[
                  {
                    key: 'team',
                    header: 'Team',
                    render: (grant) => (
                      <Link
                        to={`/settings/teams/${grant.team_id}`}
                        className="font-medium hover:underline"
                      >
                        {teamName(grant.team_id)}
                      </Link>
                    ),
                  },
                  {
                    key: 'role',
                    header: 'Role on this project',
                    render: (grant) => (
                      <>
                        <span className="capitalize">{grant.role}</span>
                        <span className="ml-2 text-xs text-muted-foreground">
                          {ROLE_DESCRIPTIONS[grant.role]}
                        </span>
                      </>
                    ),
                  },
                  {
                    key: 'actions',
                    header: <span className="sr-only">Actions</span>,
                    className: 'w-10',
                    render: (grant) => (
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={`Revoke access for ${teamName(grant.team_id)}`}
                        onClick={() => setGrantToRevoke(grant)}
                      >
                        <Trash2 className="size-4 text-destructive" />
                      </Button>
                    ),
                  },
                ]}
              />
            )}
          </div>
        </CardContent>
      </Card>

      <Dialog open={grantOpen} onOpenChange={setGrantOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Grant team access</DialogTitle>
            <DialogDescription>
              {isRestricted
                ? 'Add another team to this project.'
                : 'This is the first grant — it will restrict the project to the teams listed here.'}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-2">
            <div className="space-y-2">
              <Label htmlFor="access-team">Team</Label>
              <Select value={teamId} onValueChange={setTeamId}>
                <SelectTrigger id="access-team">
                  <SelectValue placeholder="Select a team" />
                </SelectTrigger>
                <SelectContent>
                  {availableTeams.map((team) => (
                    <SelectItem key={team.id} value={String(team.id)}>
                      {team.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {availableTeams.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  Every team already has access to this project.
                </p>
              )}
            </div>
            <div className="space-y-2">
              <Label htmlFor="access-role">Role on this project</Label>
              <RoleSelect id="access-role" value={role} onChange={setRole} />
              <p className="text-xs text-muted-foreground">
                {ROLE_DESCRIPTIONS[role]}
              </p>
              <p className="text-xs text-muted-foreground">
                {ROLE_ENFORCEMENT_NOTE}
              </p>
            </div>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setGrantOpen(false)}
              disabled={grantMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              onClick={() =>
                grantMutation.mutate({
                  path: { project_id: project.id },
                  body: { team_id: Number(teamId), role },
                })
              }
              disabled={!teamId || grantMutation.isPending}
            >
              {grantMutation.isPending ? 'Granting…' : 'Grant access'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <AlertDialog
        open={grantToRevoke !== null}
        onOpenChange={(open) => {
          if (!open) setGrantToRevoke(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Revoke access?</AlertDialogTitle>
            <AlertDialogDescription>
              Members of{' '}
              {grantToRevoke
                ? `"${teamName(grantToRevoke.team_id)}"`
                : 'this team'}{' '}
              lose access to this project immediately, unless another team also
              grants it to them.
              {grantList.length === 1 &&
                ' This is the last grant — revoking it makes the project open to everyone again.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={revokeMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (grantToRevoke) {
                  revokeMutation.mutate({
                    path: {
                      project_id: project.id,
                      team_id: grantToRevoke.team_id,
                    },
                  })
                }
              }}
              disabled={revokeMutation.isPending}
            >
              {revokeMutation.isPending ? 'Revoking…' : 'Revoke access'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

/**
 * Team detail — members and the projects this team can reach.
 *
 * Membership role and project-grant role are two different things and both
 * apply: a member's effective permissions inside a project are the
 * intersection of their role in the team and the role the team holds on
 * that project. Whichever is narrower wins, so this page shows both rather
 * than pretending there's a single "role".
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  addTeamMemberMutation,
  getProjectsOptions,
  getTeamOptions,
  getTeamQueryKey,
  listTeamMembersOptions,
  listTeamMembersQueryKey,
  listTeamProjectsOptions,
  listTeamProjectsQueryKey,
  listUsersOptions,
  listTeamsQueryKey,
  removeTeamMemberMutation,
  updateTeamMutation,
  updateTeamMemberRoleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { TeamMemberResponse, TeamRole } from '@/api/client/types.gen'
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
import { Badge } from '@/components/ui/badge'
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
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { ArrowLeft, FolderGit2, Pencil, Plus, Trash2, Users } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

const TEAM_ROLES: TeamRole[] = ['owner', 'admin', 'deployer', 'viewer']

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
  owner: 'Deploy, manage env vars, settings and domains, and delete the project',
  admin: 'Deploy, manage env vars, settings and domains. Cannot delete the project',
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

/** Shared role picker so the wording stays identical everywhere. */
export function RoleSelect({
  value,
  onChange,
  disabled,
  id,
}: {
  value: TeamRole
  onChange: (role: TeamRole) => void
  disabled?: boolean
  id?: string
}) {
  return (
    <Select
      value={value}
      onValueChange={(v) => onChange(v as TeamRole)}
      disabled={disabled}
    >
      <SelectTrigger id={id}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {TEAM_ROLES.map((role) => (
          <SelectItem key={role} value={role}>
            <span className="capitalize">{role}</span>
            <span className="ml-2 text-xs text-muted-foreground">
              {ROLE_DESCRIPTIONS[role]}
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

interface AddMemberDialogProps {
  teamId: number
  existingUserIds: number[]
  open: boolean
  onOpenChange: (open: boolean) => void
}

function AddMemberDialog({
  teamId,
  existingUserIds,
  open,
  onOpenChange,
}: AddMemberDialogProps) {
  const queryClient = useQueryClient()
  const [userId, setUserId] = useState<string>('')
  const [role, setRole] = useState<TeamRole>('viewer')

  const { data: users, isLoading: usersLoading } = useQuery(
    listUsersOptions({ query: { include_deleted: false } })
  )

  // Users already on the team would just 409 — leave them out of the picker.
  const available = useMemo(
    () => (users ?? []).filter((u) => !existingUserIds.includes(u.user.id)),
    [users, existingUserIds]
  )

  const addMutation = useMutation({
    ...addTeamMemberMutation(),
    onSuccess: (member) => {
      queryClient.invalidateQueries({
        queryKey: listTeamMembersQueryKey({ path: { team_id: teamId } }),
      })
      toast.success(
        `${member?.user_email ?? 'Member'} added as ${member?.role ?? role}`
      )
      onOpenChange(false)
      setUserId('')
      setRole('viewer')
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to add member')
    },
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add member</DialogTitle>
          <DialogDescription>
            Their role here is capped by the role this team holds on each
            project — the narrower of the two applies.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <Label htmlFor="member-user">User</Label>
            {usersLoading ? (
              <Skeleton className="h-9 w-full" />
            ) : (
              <Select value={userId} onValueChange={setUserId}>
                <SelectTrigger id="member-user">
                  <SelectValue placeholder="Select a user" />
                </SelectTrigger>
                <SelectContent>
                  {available.map((u) => (
                    <SelectItem key={u.user.id} value={String(u.user.id)}>
                      {u.user.name} ({u.user.email})
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            {!usersLoading && available.length === 0 && (
              <p className="text-xs text-muted-foreground">
                Every user is already a member of this team.
              </p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor="member-role">Role</Label>
            <RoleSelect id="member-role" value={role} onChange={setRole} />
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
            onClick={() => onOpenChange(false)}
            disabled={addMutation.isPending}
          >
            Cancel
          </Button>
          <Button
            onClick={() =>
              addMutation.mutate({
                path: { team_id: teamId },
                body: { user_id: Number(userId), role },
              })
            }
            disabled={!userId || addMutation.isPending}
          >
            {addMutation.isPending ? 'Adding…' : 'Add member'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

interface EditTeamDialogProps {
  teamId: number
  name: string
  description: string | null | undefined
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * Rename a team / change its description. The slug is deliberately absent:
 * it's the stable handle the CLI and scripts address the team by, so it is
 * fixed at creation rather than quietly editable here.
 */
function EditTeamDialog({
  teamId,
  name,
  description,
  open,
  onOpenChange,
}: EditTeamDialogProps) {
  const queryClient = useQueryClient()
  const [draftName, setDraftName] = useState(name)
  const [draftDescription, setDraftDescription] = useState(description ?? '')

  // The dialog stays mounted (no conditional mounting), so re-seed the
  // fields whenever it opens or the underlying team changes.
  useEffect(() => {
    if (!open) return
    setDraftName(name)
    setDraftDescription(description ?? '')
  }, [open, name, description])

  const updateMutation = useMutation({
    ...updateTeamMutation(),
    onSuccess: (updated) => {
      queryClient.invalidateQueries({
        queryKey: getTeamQueryKey({ path: { team_id: teamId } }),
      })
      queryClient.invalidateQueries({ queryKey: listTeamsQueryKey() })
      toast.success(`Team "${updated?.name ?? draftName}" updated`)
      onOpenChange(false)
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to update team')
    },
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Edit team</DialogTitle>
          <DialogDescription>
            Changing the name doesn't affect who can reach what — membership
            and project grants stay as they are.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <Label htmlFor="edit-team-name">Name</Label>
            <Input
              id="edit-team-name"
              value={draftName}
              onChange={(e) => setDraftName(e.target.value)}
              autoFocus
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="edit-team-description">Description</Label>
            <Input
              id="edit-team-description"
              value={draftDescription}
              onChange={(e) => setDraftDescription(e.target.value)}
              placeholder="What this team is for"
            />
          </div>
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={updateMutation.isPending}
          >
            Cancel
          </Button>
          <Button
            onClick={() =>
              updateMutation.mutate({
                path: { team_id: teamId },
                body: {
                  name: draftName.trim(),
                  description: draftDescription.trim() || null,
                },
              })
            }
            disabled={!draftName.trim() || updateMutation.isPending}
          >
            {updateMutation.isPending ? 'Saving…' : 'Save changes'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** Inline role editor for one membership. */
function MemberRoleCell({
  teamId,
  member,
}: {
  teamId: number
  member: TeamMemberResponse
}) {
  const queryClient = useQueryClient()

  const updateMutation = useMutation({
    ...updateTeamMemberRoleMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listTeamMembersQueryKey({ path: { team_id: teamId } }),
      })
      toast.success('Role updated')
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to update role')
    },
  })

  return (
    <RoleSelect
      value={member.role}
      disabled={updateMutation.isPending}
      onChange={(role) =>
        updateMutation.mutate({
          path: { team_id: teamId, user_id: member.user_id },
          body: { role },
        })
      }
    />
  )
}

function MembersSkeleton() {
  return (
    <div className="space-y-2">
      {[0, 1, 2].map((i) => (
        <Skeleton key={i} className="h-12 w-full" />
      ))}
    </div>
  )
}

export function TeamDetail() {
  const { teamId: teamIdParam } = useParams<{ teamId: string }>()
  const teamId = Number(teamIdParam)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [addOpen, setAddOpen] = useState(false)
  const [editOpen, setEditOpen] = useState(false)
  const [memberToRemove, setMemberToRemove] =
    useState<TeamMemberResponse | null>(null)

  const { data: team, isLoading: teamLoading } = useQuery({
    ...getTeamOptions({ path: { team_id: teamId } }),
    enabled: Number.isFinite(teamId),
  })
  const { data: members, isLoading: membersLoading } = useQuery({
    ...listTeamMembersOptions({ path: { team_id: teamId } }),
    enabled: Number.isFinite(teamId),
  })
  const { data: grants, isLoading: grantsLoading } = useQuery({
    ...listTeamProjectsOptions({ path: { team_id: teamId } }),
    enabled: Number.isFinite(teamId),
  })
  // Grants carry project ids only; the list gives them names.
  const { data: projects } = useQuery(
    getProjectsOptions({ query: { per_page: 100 } })
  )

  usePageTitle(team?.name ? `Team · ${team.name}` : 'Team')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Teams', href: '/settings/teams' },
      { label: team?.name ?? 'Team' },
    ])
  }, [setBreadcrumbs, team?.name])

  const removeMutation = useMutation({
    ...removeTeamMemberMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listTeamMembersQueryKey({ path: { team_id: teamId } }),
      })
      queryClient.invalidateQueries({
        queryKey: listTeamProjectsQueryKey({ path: { team_id: teamId } }),
      })
      queryClient.invalidateQueries({
        queryKey: getTeamQueryKey({ path: { team_id: teamId } }),
      })
      toast.success('Member removed')
      setMemberToRemove(null)
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to remove member')
    },
  })

  // Grants store project ids, but the project route is keyed by slug —
  // linking by id 404s. Fall back to showing the id un-linked when the
  // project isn't in the caller's own visible list.
  const projectFor = (id: number) => projects?.projects?.find((p) => p.id === id)
  const projectName = (id: number) => projectFor(id)?.name ?? `Project ${id}`

  const memberList = members ?? []
  const grantList = grants ?? []

  return (
    <div className="container mx-auto space-y-6 px-4 py-6 sm:px-6">
      <div className="flex items-center gap-2">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate('/settings/teams')}
        >
          <ArrowLeft className="mr-2 h-4 w-4" />
          Back to teams
        </Button>
      </div>

      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div>
          {teamLoading ? (
            <Skeleton className="h-9 w-64" />
          ) : (
            <h1 className="text-2xl font-bold sm:text-3xl">
              {team?.name ?? 'Team'}
            </h1>
          )}
          <p className="mt-2 text-muted-foreground">
            {team?.description || 'No description'}
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => setEditOpen(true)}
          disabled={!team}
        >
          <Pencil className="mr-2 h-4 w-4" />
          Edit team
        </Button>
      </div>

      <Card>
        <CardHeader className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              <Users className="h-4 w-4" />
              Members
            </CardTitle>
            <CardDescription>
              Everyone here reaches every project granted to this team.
            </CardDescription>
          </div>
          <Button size="sm" onClick={() => setAddOpen(true)}>
            <Plus className="mr-2 h-4 w-4" />
            Add member
          </Button>
        </CardHeader>
        <CardContent>
          {membersLoading ? (
            <MembersSkeleton />
          ) : memberList.length === 0 ? (
            <EmptyState
              icon={Users}
              title="No members"
              description="Add users to this team so they inherit its project access."
              action={
                <Button onClick={() => setAddOpen(true)}>
                  <Plus className="mr-2 h-4 w-4" />
                  Add member
                </Button>
              }
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Email
                    </TableHead>
                    <TableHead>Role in team</TableHead>
                    <TableHead className="w-10" />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {memberList.map((member) => (
                    <TableRow key={member.id}>
                      <TableCell className="font-medium">
                        {member.user_name ?? `User ${member.user_id}`}
                      </TableCell>
                      <TableCell className="hidden text-muted-foreground md:table-cell">
                        {member.user_email ?? '—'}
                      </TableCell>
                      <TableCell>
                        <MemberRoleCell teamId={teamId} member={member} />
                      </TableCell>
                      <TableCell>
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={`Remove ${member.user_name ?? 'member'}`}
                          onClick={() => setMemberToRemove(member)}
                        >
                          <Trash2 className="h-4 w-4 text-destructive" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <FolderGit2 className="h-4 w-4" />
            Projects
          </CardTitle>
          <CardDescription>
            Grants are added from each project's Access tab.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {grantsLoading ? (
            <MembersSkeleton />
          ) : grantList.length === 0 ? (
            <EmptyState
              icon={FolderGit2}
              title="No project access"
              description="This team can't reach any project yet. Open a project's Access tab to grant it."
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Project</TableHead>
                    <TableHead>Team's role on it</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {grantList.map((grant) => (
                    <TableRow
                      key={grant.id}
                      className={
                        projectFor(grant.project_id) ? 'cursor-pointer' : undefined
                      }
                      onClick={() => {
                        const slug = projectFor(grant.project_id)?.slug
                        if (slug) navigate(`/projects/${slug}`)
                      }}
                    >
                      <TableCell className="font-medium">
                        {projectName(grant.project_id)}
                      </TableCell>
                      <TableCell>
                        <Badge variant="outline" className="capitalize">
                          {grant.role}
                        </Badge>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
      </Card>

      <EditTeamDialog
        teamId={teamId}
        name={team?.name ?? ''}
        description={team?.description}
        open={editOpen}
        onOpenChange={setEditOpen}
      />

      <AddMemberDialog
        teamId={teamId}
        existingUserIds={memberList.map((m) => m.user_id)}
        open={addOpen}
        onOpenChange={setAddOpen}
      />

      <AlertDialog
        open={memberToRemove !== null}
        onOpenChange={(open) => {
          if (!open) setMemberToRemove(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove member?</AlertDialogTitle>
            <AlertDialogDescription>
              {memberToRemove?.user_name ?? 'This user'} loses access to every
              project they could only reach through this team. This takes
              effect immediately.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={removeMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (memberToRemove) {
                  removeMutation.mutate({
                    path: {
                      team_id: teamId,
                      user_id: memberToRemove.user_id,
                    },
                  })
                }
              }}
              disabled={removeMutation.isPending}
            >
              {removeMutation.isPending ? 'Removing…' : 'Remove'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Teams — group users and grant them scoped access to projects.
 *
 * A project with no team grants is reachable by everyone with the relevant
 * instance permission; adding the first grant is what restricts it. The
 * copy on this page leans on that, because it's the part operators get
 * wrong: creating teams alone changes nothing.
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  createTeamMutation,
  deleteTeamMutation,
  listTeamMembersOptions,
  listTeamsOptions,
  listTeamsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { TeamResponse } from '@/api/client/types.gen'
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
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CreateActionButton } from '@/components/ui/create-action-button'
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { Plus, Trash2, Users } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'

/** Slugify a team name into the `[a-z0-9-]+` shape the backend validates. */
function slugify(name: string): string {
  return name
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
}

/** Member count for one team — its own query so the table can show it inline. */
function MemberCount({ teamId }: { teamId: number }) {
  const { data, isLoading } = useQuery(
    listTeamMembersOptions({ path: { team_id: teamId } })
  )
  if (isLoading) return <Skeleton className="h-4 w-8" />
  return <span className="tabular-nums">{data?.length ?? 0}</span>
}

interface CreateTeamDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

function CreateTeamDialog({ open, onOpenChange }: CreateTeamDialogProps) {
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [slug, setSlug] = useState('')
  const [slugTouched, setSlugTouched] = useState(false)
  const [description, setDescription] = useState('')

  const effectiveSlug = slugTouched ? slug : slugify(name)

  const createMutation = useMutation({
    ...createTeamMutation(),
    onSuccess: (team) => {
      queryClient.invalidateQueries({ queryKey: listTeamsQueryKey() })
      toast.success(`Team "${team?.name}" created`)
      onOpenChange(false)
      setName('')
      setSlug('')
      setSlugTouched(false)
      setDescription('')
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to create team')
    },
  })

  const handleSubmit = () => {
    createMutation.mutate({
      body: {
        name: name.trim(),
        slug: effectiveSlug,
        description: description.trim() || null,
      },
    })
  }

  const canSubmit = name.trim().length > 0 && effectiveSlug.length > 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Create team</DialogTitle>
          <DialogDescription>
            Teams group users. Once created, grant the team access to the
            projects its members should reach.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <Label htmlFor="team-name">Name</Label>
            <Input
              id="team-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Platform Engineering"
              autoFocus
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="team-slug">Slug</Label>
            <Input
              id="team-slug"
              value={effectiveSlug}
              onChange={(e) => {
                setSlugTouched(true)
                setSlug(slugify(e.target.value))
              }}
              placeholder="platform-engineering"
            />
            <p className="text-xs text-muted-foreground">
              Lowercase letters, numbers and hyphens. Auto-derived from the
              name until edited.
            </p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="team-description">Description (optional)</Label>
            <Input
              id="team-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Owns the deployment platform"
            />
          </div>
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={createMutation.isPending}
          >
            Cancel
          </Button>
          <Button
            onClick={handleSubmit}
            disabled={!canSubmit || createMutation.isPending}
          >
            {createMutation.isPending ? 'Creating…' : 'Create team'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function TeamsTableSkeleton() {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Slug</TableHead>
          <TableHead className="hidden md:table-cell">Description</TableHead>
          <TableHead className="text-right">Members</TableHead>
          <TableHead className="w-10" />
        </TableRow>
      </TableHeader>
      <TableBody>
        {[0, 1, 2].map((i) => (
          <TableRow key={i}>
            <TableCell>
              <Skeleton className="h-4 w-32" />
            </TableCell>
            <TableCell>
              <Skeleton className="h-4 w-24" />
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-48" />
            </TableCell>
            <TableCell className="text-right">
              <Skeleton className="ml-auto h-4 w-8" />
            </TableCell>
            <TableCell>
              <Skeleton className="h-8 w-8" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

export function Teams() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const [teamToDelete, setTeamToDelete] = useState<TeamResponse | null>(null)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  usePageTitle('Teams')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Teams' }])
  }, [setBreadcrumbs])

  // The URL owns the dialog, so the command palette (and any deep link) can
  // open it with `?new=1` — there is no /teams/new route, creation is a
  // dialog. Deriving it beats mirroring the param into state via an effect:
  // no cascading render, and it still reacts when the palette navigates here
  // while this page is already mounted.
  const createOpen = searchParams.get('new') === '1'
  const setCreateOpen = (open: boolean) => {
    const next = new URLSearchParams(searchParams)
    if (open) next.set('new', '1')
    else next.delete('new')
    setSearchParams(next, { replace: !open })
  }

  const { data, isLoading, isError, error } = useQuery(
    listTeamsOptions({ query: { page: 1, page_size: 100 } })
  )

  const deleteMutation = useMutation({
    ...deleteTeamMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: listTeamsQueryKey() })
      toast.success(`Team "${teamToDelete?.name}" deleted`)
      setTeamToDelete(null)
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          deleteMutation.mutate(variables)
        )
      ) {
        setTeamToDelete(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(problem.detail || problem.message || 'Failed to delete team')
    },
  })

  const teams = data?.teams ?? []

  return (
    <div className="container mx-auto space-y-6 px-4 py-6 sm:px-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-bold sm:text-3xl">Teams</h1>
          <p className="mt-2 text-muted-foreground">
            Group users, then grant each team access to the projects it
            should reach. Projects with no grants stay open to everyone.
          </p>
        </div>
        <CreateActionButton
          onClick={() => setCreateOpen(true)}
          label="Create team"
          className="self-start"
        />
      </div>

      <Card>
        <CardContent className="pt-6">
          {isLoading ? (
            <TeamsTableSkeleton />
          ) : isError ? (
            <EmptyState
              icon={Users}
              title="Couldn't load teams"
              description={
                error instanceof Error
                  ? error.message
                  : 'The teams API did not respond.'
              }
            />
          ) : teams.length === 0 ? (
            <EmptyState
              icon={Users}
              title="No teams yet"
              description="Create a team, add members, then grant it access to a project from that project's Access tab."
              action={
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus className="mr-2 h-4 w-4" />
                  Create team
                </Button>
              }
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead>Slug</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Description
                    </TableHead>
                    <TableHead className="text-right">Members</TableHead>
                    <TableHead className="w-10" />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {teams.map((team) => (
                    <TableRow
                      key={team.id}
                      className="cursor-pointer"
                      onClick={() => navigate(`/settings/teams/${team.id}`)}
                    >
                      <TableCell className="font-medium">{team.name}</TableCell>
                      <TableCell>
                        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                          {team.slug}
                        </code>
                      </TableCell>
                      <TableCell className="hidden text-muted-foreground md:table-cell">
                        {team.description || '—'}
                      </TableCell>
                      <TableCell className="text-right">
                        <MemberCount teamId={team.id} />
                      </TableCell>
                      <TableCell>
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={`Delete team ${team.name}`}
                          onClick={(e) => {
                            // Don't trigger the row's navigate.
                            e.stopPropagation()
                            setTeamToDelete(team)
                          }}
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

      <CreateTeamDialog open={createOpen} onOpenChange={setCreateOpen} />

      {verificationDialog}

      <AlertDialog
        open={teamToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setTeamToDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete team?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes the team
              {teamToDelete ? ` "${teamToDelete.name}"` : ''} along with its
              memberships and project-access grants. Members lose access to
              every project they could only reach through this team. This
              cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (teamToDelete) {
                  deleteMutation.mutate({
                    path: { team_id: teamToDelete.id },
                  })
                }
              }}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete team'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

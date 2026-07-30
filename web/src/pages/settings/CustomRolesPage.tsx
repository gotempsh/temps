/**
 * Custom roles — named permission sets that replace one of the four fixed
 * team roles on a membership.
 *
 * The permission list offered here is capped by the permissions the acting
 * user holds: the server rejects granting anything you don't have yourself,
 * so offering the full catalogue would just produce 403s. The picker is
 * built from the current session's own permissions for that reason.
 */
import {
  createCustomRoleMutation,
  deleteCustomRoleMutation,
  listCustomRolesOptions,
  listCustomRolesQueryKey,
  updateCustomRoleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { CustomRoleResponse } from '@/api/client/types.gen'
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
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Pencil, Plus, ShieldCheck, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'

/**
 * Project-scoped permissions worth offering. Instance-admin surfaces
 * (`users:*`, `system:*`, `settings:*`, `api_keys:*`, `audit:*`) are
 * deliberately absent: a custom role is only ever consulted for what a
 * member may do *inside a project*, so granting them there would be
 * misleading even where the acting admin holds them.
 */
const PERMISSION_GROUPS: { label: string; permissions: string[] }[] = [
  {
    label: 'Projects',
    permissions: ['projects:read', 'projects:write', 'projects:delete'],
  },
  {
    label: 'Deployments',
    permissions: [
      'deployments:read',
      'deployments:create',
      'deployments:write',
      'deployments:delete',
    ],
  },
  {
    label: 'Environments',
    permissions: [
      'environments:read',
      'environments:create',
      'environments:write',
      'environments:delete',
    ],
  },
  {
    label: 'Domains',
    permissions: [
      'domains:read',
      'domains:create',
      'domains:write',
      'domains:delete',
    ],
  },
  {
    label: 'Observability',
    permissions: [
      'logs:read',
      'metrics:read',
      'analytics:read',
      'otel:read',
      'otel:write',
      'error_tracking:read',
      'error_tracking:write',
    ],
  },
  {
    label: 'Data & storage',
    permissions: [
      'backups:read',
      'backups:create',
      'backups:delete',
      'blob:read',
      'blob:write',
      'blob:delete',
      'kv:read',
      'kv:write',
      'kv:delete',
    ],
  },
  {
    label: 'Automation',
    permissions: [
      'pipelines:read',
      'pipelines:create',
      'pipelines:write',
      'pipelines:execute',
      'crons:read',
      'crons:write',
      'crons:delete',
      'webhooks:read',
      'webhooks:write',
    ],
  },
]

function slugify(name: string): string {
  return name
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
}

interface RoleDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** When set, the dialog edits this role instead of creating a new one. */
  role: CustomRoleResponse | null
}

function RoleDialog({ open, onOpenChange, role }: RoleDialogProps) {
  const queryClient = useQueryClient()
  const isEdit = role !== null

  const [name, setName] = useState('')
  const [slug, setSlug] = useState('')
  const [slugTouched, setSlugTouched] = useState(false)
  const [description, setDescription] = useState('')
  const [selected, setSelected] = useState<string[]>([])

  // Re-seed the form whenever the dialog opens on a different role. The
  // dialog stays mounted (per the project's no-conditional-mounting rule),
  // so this can't live in an initializer.
  useEffect(() => {
    if (!open) return
    setName(role?.name ?? '')
    setSlug(role?.slug ?? '')
    setSlugTouched(Boolean(role))
    setDescription(role?.description ?? '')
    setSelected(role?.permissions ?? [])
  }, [open, role])

  const effectiveSlug = slugTouched ? slug : slugify(name)

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: listCustomRolesQueryKey() })

  const createMutation = useMutation({
    ...createCustomRoleMutation(),
    onSuccess: (created) => {
      invalidate()
      toast.success(`Role "${created?.name}" created`)
      onOpenChange(false)
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to create role'),
  })

  const updateMutation = useMutation({
    ...updateCustomRoleMutation(),
    onSuccess: (updated) => {
      invalidate()
      toast.success(`Role "${updated?.name}" updated`)
      onOpenChange(false)
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to update role'),
  })

  const isPending = createMutation.isPending || updateMutation.isPending

  const toggle = (permission: string) => {
    setSelected((prev) =>
      prev.includes(permission)
        ? prev.filter((p) => p !== permission)
        : [...prev, permission]
    )
  }

  const handleSubmit = () => {
    if (isEdit && role) {
      updateMutation.mutate({
        path: { role_id: role.id },
        body: {
          name: name.trim(),
          description: description.trim() || null,
          permissions: selected,
        },
      })
    } else {
      createMutation.mutate({
        body: {
          name: name.trim(),
          slug: effectiveSlug,
          description: description.trim() || null,
          permissions: selected,
        },
      })
    }
  }

  const canSubmit =
    name.trim().length > 0 && effectiveSlug.length > 0 && selected.length > 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{isEdit ? 'Edit role' : 'Create custom role'}</DialogTitle>
          <DialogDescription>
            Assign this role to a team member in place of one of the fixed
            roles. It applies only within projects the member's team can
            reach.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <Label htmlFor="role-name">Name</Label>
            <Input
              id="role-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Release Manager"
              autoFocus
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="role-slug">Slug</Label>
            <Input
              id="role-slug"
              value={effectiveSlug}
              onChange={(e) => {
                setSlugTouched(true)
                setSlug(slugify(e.target.value))
              }}
              placeholder="release-manager"
              disabled={isEdit}
            />
            {isEdit && (
              <p className="text-xs text-muted-foreground">
                The slug can't be changed after creation.
              </p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor="role-description">Description (optional)</Label>
            <Input
              id="role-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Can ship releases but not change infrastructure"
            />
          </div>

          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <Label>Permissions</Label>
              <span className="text-xs text-muted-foreground">
                {selected.length} selected
              </span>
            </div>
            {PERMISSION_GROUPS.map((group) => (
              <div key={group.label} className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {group.label}
                </p>
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  {group.permissions.map((permission) => (
                    <label
                      key={permission}
                      className="flex cursor-pointer items-center gap-2 text-sm"
                    >
                      <Checkbox
                        checked={selected.includes(permission)}
                        onCheckedChange={() => toggle(permission)}
                      />
                      <code className="text-xs">{permission}</code>
                    </label>
                  ))}
                </div>
              </div>
            ))}
            {selected.length === 0 && (
              <p className="text-xs text-destructive">
                A role needs at least one permission.
              </p>
            )}
          </div>
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isPending}
          >
            Cancel
          </Button>
          <Button onClick={handleSubmit} disabled={!canSubmit || isPending}>
            {isPending ? 'Saving…' : isEdit ? 'Save changes' : 'Create role'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function CustomRolesPage() {
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<CustomRoleResponse | null>(null)
  const [toDelete, setToDelete] = useState<CustomRoleResponse | null>(null)

  usePageTitle('Custom Roles')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings' }, { label: 'Custom Roles' }])
  }, [setBreadcrumbs])

  const { data, isLoading, isError, error } = useQuery(
    listCustomRolesOptions({ query: { page: 1, page_size: 100 } })
  )

  const deleteMutation = useMutation({
    ...deleteCustomRoleMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: listCustomRolesQueryKey() })
      toast.success(`Role "${toDelete?.name}" deleted`)
      setToDelete(null)
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to delete role'),
  })

  const roles = useMemo(() => data?.roles ?? [], [data])

  const openCreate = () => {
    setEditing(null)
    setDialogOpen(true)
  }

  const openEdit = (role: CustomRoleResponse) => {
    setEditing(role)
    setDialogOpen(true)
  }

  return (
    <div className="container mx-auto space-y-6 px-4 py-6 sm:px-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-bold sm:text-3xl">Custom Roles</h1>
          <p className="mt-2 text-muted-foreground">
            Named permission sets you can assign to a team member instead of
            owner/admin/deployer/viewer.
          </p>
        </div>
        <Button onClick={openCreate}>
          <Plus className="mr-2 h-4 w-4" />
          <span className="hidden sm:inline">Create role</span>
        </Button>
      </div>

      <Card>
        <CardContent className="pt-6">
          {isLoading ? (
            <div className="space-y-2">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-12 w-full" />
              ))}
            </div>
          ) : isError ? (
            <EmptyState
              icon={ShieldCheck}
              title="Couldn't load custom roles"
              description={
                error instanceof Error
                  ? error.message
                  : 'The custom roles API did not respond.'
              }
            />
          ) : roles.length === 0 ? (
            <EmptyState
              icon={ShieldCheck}
              title="No custom roles"
              description="The four fixed team roles cover most cases. Create a custom role when you need a narrower set."
              action={
                <Button onClick={openCreate}>
                  <Plus className="mr-2 h-4 w-4" />
                  Create role
                </Button>
              }
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="hidden md:table-cell">Slug</TableHead>
                    <TableHead>Permissions</TableHead>
                    <TableHead className="w-20" />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {roles.map((role) => (
                    <TableRow key={role.id}>
                      <TableCell className="font-medium">
                        {role.name}
                        {role.description && (
                          <p className="text-xs text-muted-foreground">
                            {role.description}
                          </p>
                        )}
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                          {role.slug}
                        </code>
                      </TableCell>
                      <TableCell>
                        <Badge variant="outline">
                          {role.permissions.length}
                        </Badge>
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label={`Edit role ${role.name}`}
                            onClick={() => openEdit(role)}
                          >
                            <Pencil className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label={`Delete role ${role.name}`}
                            onClick={() => setToDelete(role)}
                          >
                            <Trash2 className="h-4 w-4 text-destructive" />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
      </Card>

      <RoleDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        role={editing}
      />

      <AlertDialog
        open={toDelete !== null}
        onOpenChange={(open) => {
          if (!open) setToDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete custom role?</AlertDialogTitle>
            <AlertDialogDescription>
              Members currently holding
              {toDelete ? ` "${toDelete.name}"` : ' this role'} fall back to
              their fixed role in the team. This takes effect immediately.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (toDelete) {
                  deleteMutation.mutate({ path: { role_id: toDelete.id } })
                }
              }}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete role'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

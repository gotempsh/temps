// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import {
  listDeploymentTokensOptions,
  listDeploymentTokensQueryKey,
  createDeploymentTokenMutation,
  deleteDeploymentTokenMutation,
  getEnvironmentsOptions,
  rotateDeploymentTokenMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
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
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Skeleton } from '@/components/ui/skeleton'
import { EmptyState } from '@/components/ui/empty-state'
import { CopyButton } from '@/components/ui/copy-button'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, RefreshCw, Trash2, Key, Plus } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import type { DeploymentTokenResponse } from '@/api/client/types.gen'
import {
  DEPLOYMENT_TOKEN_PERMISSIONS,
  deploymentTokenErrorMessage,
  validateDeploymentTokenInput,
} from './deployment-token-form'

interface DeploymentTokensSettingsProps {
  project: ProjectResponse
}

export function DeploymentTokensSettings({
  project,
}: DeploymentTokensSettingsProps) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const [deleteTarget, setDeleteTarget] =
    useState<DeploymentTokenResponse | null>(null)
  const [rotateTarget, setRotateTarget] =
    useState<DeploymentTokenResponse | null>(null)
  const [createOpen, setCreateOpen] = useState(false)
  const [tokenName, setTokenName] = useState('')
  const [environmentId, setEnvironmentId] = useState('all')
  const [expiresAt, setExpiresAt] = useState('')
  const [permissions, setPermissions] = useState<string[]>([])
  const [createError, setCreateError] = useState<string | null>(null)
  // After a successful rotation the new plaintext token is stored here (shown once).
  const [revealedToken, setRevealedToken] = useState<{
    id: number
    value: string
  } | null>(null)

  const tokensQuery = useQuery({
    ...listDeploymentTokensOptions({
      path: { project_id: project.id },
      query: { page: 1, page_size: 100 },
    }),
  })

  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
  })

  const createMutation = useMutation({
    ...createDeploymentTokenMutation(),
    onSuccess: (data) => {
      setRevealedToken({ id: data.id, value: data.token })
      setCreateOpen(false)
      setTokenName('')
      setEnvironmentId('all')
      setExpiresAt('')
      setPermissions([])
      setCreateError(null)
      toast.success(
        'Deployment token created — copy it now, it will not be shown again'
      )
      queryClient.invalidateQueries({
        queryKey: listDeploymentTokensQueryKey({
          path: { project_id: project.id },
          query: { page: 1, page_size: 100 },
        }),
      })
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          createMutation.mutate(variables)
        )
      ) {
        setCreateError(null)
        return
      }
      setCreateError(deploymentTokenErrorMessage(error))
    },
  })

  const submitCreate = () => {
    const validationError = validateDeploymentTokenInput(
      tokenName,
      expiresAt,
      permissions
    )
    if (validationError) {
      setCreateError(validationError)
      return
    }
    setCreateError(null)
    createMutation.mutate({
      path: { project_id: project.id },
      body: {
        name: tokenName.trim(),
        environment_id: environmentId === 'all' ? null : Number(environmentId),
        expires_at: expiresAt ? new Date(expiresAt).toISOString() : null,
        permissions,
      },
    })
  }

  const togglePermission = (permission: string, checked: boolean) => {
    setPermissions((current) => {
      if (!checked) return current.filter((item) => item !== permission)
      if (permission === '*') return ['*']
      return [...current.filter((item) => item !== '*'), permission]
    })
  }

  useEffect(() => {
    // Plaintext credentials belong to one project and must never remain on
    // screen after project navigation reuses this settings component.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealedToken(null)
  }, [project.id])

  const openCreate = () => {
    setRevealedToken(null)
    setCreateError(null)
    setCreateOpen(true)
  }

  const deleteMutation = useMutation({
    ...deleteDeploymentTokenMutation(),
    onSuccess: () => {
      toast.success('Deployment token deleted')
      setDeleteTarget(null)
      queryClient.invalidateQueries({
        queryKey: listDeploymentTokensQueryKey({
          path: { project_id: project.id },
          query: { page: 1, page_size: 100 },
        }),
      })
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          deleteMutation.mutate(variables)
        )
      ) {
        setDeleteTarget(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(
        problem.detail || problem.message || 'Failed to delete deployment token'
      )
    },
  })

  const rotateMutation = useMutation({
    ...rotateDeploymentTokenMutation(),
    onSuccess: (data) => {
      toast.success(
        'Deployment token rotated — copy the new token now, it will not be shown again'
      )
      setRotateTarget(null)
      setRevealedToken({ id: data.id, value: data.token })
      queryClient.invalidateQueries({
        queryKey: listDeploymentTokensQueryKey({
          path: { project_id: project.id },
          query: { page: 1, page_size: 100 },
        }),
      })
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          rotateMutation.mutate(variables)
        )
      ) {
        setRotateTarget(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(
        problem.detail || problem.message || 'Failed to rotate deployment token'
      )
    },
  })

  const tokens = tokensQuery.data?.tokens ?? []

  return (
    <div className="space-y-6">
      {verificationDialog}

      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4 border-b">
          <div className="space-y-1.5">
            <CardTitle className="flex items-center gap-2">
              <Key className="h-5 w-5" />
              Deployment Tokens
            </CardTitle>
            <CardDescription>
              Deployment tokens provide <code>TEMPS_API_URL</code> and{' '}
              <code>TEMPS_API_TOKEN</code> credentials that are automatically
              injected into deployed applications.
            </CardDescription>
          </div>
          {tokens.length > 0 && (
            <Button size="sm" onClick={openCreate}>
              <Plus className="mr-2 h-4 w-4" /> Create token
            </Button>
          )}
        </CardHeader>
        <CardContent className="pt-6">
          {revealedToken && (
            <div className="mb-4 rounded-md border border-amber-300 bg-amber-50 p-4 dark:border-amber-800 dark:bg-amber-950/40">
              <p className="mb-2 text-sm font-medium text-amber-800 dark:text-amber-200">
                New token value — copy it now, it will not be shown again:
              </p>
              <div className="flex items-center gap-2">
                <code className="flex-1 text-xs break-all font-mono">
                  {revealedToken.value}
                </code>
                <CopyButton value={revealedToken.value} />
              </div>
              <Button
                size="sm"
                variant="ghost"
                className="mt-2 text-xs text-amber-700 dark:text-amber-300"
                onClick={() => setRevealedToken(null)}
              >
                I have saved the token
              </Button>
            </div>
          )}

          {tokensQuery.isLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
            </div>
          ) : tokensQuery.isError ? (
            <p className="text-sm text-destructive">
              Failed to load deployment tokens.
            </p>
          ) : tokens.length === 0 ? (
            <EmptyState
              size="compact"
              icon={Key}
              title="No deployment tokens yet"
              description={
                <p className="text-sm text-muted-foreground">
                  Create a scoped credential for an application or integration.
                </p>
              }
              action={<Button onClick={openCreate}>Create token</Button>}
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Prefix
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Status
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Last used
                    </TableHead>
                    <TableHead className="text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {tokens.map((token) => (
                    <TableRow key={token.id}>
                      <TableCell className="font-medium">
                        {token.name}
                      </TableCell>
                      <TableCell className="hidden md:table-cell font-mono text-xs">
                        {token.token_prefix}…
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <Badge
                          variant={token.is_active ? 'default' : 'secondary'}
                        >
                          {token.is_active ? 'Active' : 'Inactive'}
                        </Badge>
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-sm text-muted-foreground">
                        {token.last_used_at
                          ? new Date(token.last_used_at).toLocaleDateString()
                          : 'Never'}
                      </TableCell>
                      <TableCell className="text-right">
                        <div className="flex justify-end gap-2">
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() => setRotateTarget(token)}
                            disabled={
                              rotateMutation.isPending &&
                              rotateTarget?.id === token.id
                            }
                          >
                            {rotateMutation.isPending &&
                            rotateTarget?.id === token.id ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <RefreshCw className="h-4 w-4" />
                            )}
                            <span className="hidden sm:inline ml-1">
                              Rotate
                            </span>
                          </Button>
                          <Button
                            size="sm"
                            variant="destructive"
                            onClick={() => setDeleteTarget(token)}
                            disabled={
                              deleteMutation.isPending &&
                              deleteTarget?.id === token.id
                            }
                          >
                            {deleteMutation.isPending &&
                            deleteTarget?.id === token.id ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <Trash2 className="h-4 w-4" />
                            )}
                            <span className="hidden sm:inline ml-1">
                              Delete
                            </span>
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

      <Dialog
        open={createOpen}
        onOpenChange={(open) =>
          !createMutation.isPending && setCreateOpen(open)
        }
      >
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Create deployment token</DialogTitle>
            <DialogDescription>
              Choose the narrowest scope your server-side application needs. The
              secret is shown once.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-5 py-2">
            <div className="space-y-2">
              <Label htmlFor="deployment-token-name">Name</Label>
              <Input
                id="deployment-token-name"
                value={tokenName}
                onChange={(event) => setTokenName(event.target.value)}
                placeholder="Production worker"
                autoFocus
              />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label>Environment</Label>
                <Select
                  value={environmentId}
                  onValueChange={setEnvironmentId}
                  disabled={environmentsQuery.isLoading}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="All environments" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All environments</SelectItem>
                    {(environmentsQuery.data ?? []).map((environment) => (
                      <SelectItem
                        key={environment.id}
                        value={String(environment.id)}
                      >
                        {environment.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-2">
                <Label htmlFor="deployment-token-expiry">
                  Expires (optional)
                </Label>
                <Input
                  id="deployment-token-expiry"
                  type="datetime-local"
                  value={expiresAt}
                  onChange={(event) => setExpiresAt(event.target.value)}
                />
              </div>
            </div>
            <fieldset className="space-y-3">
              <legend className="text-sm font-medium">Permissions</legend>
              <div className="grid max-h-64 gap-3 overflow-y-auto rounded-md border p-3 sm:grid-cols-2">
                {DEPLOYMENT_TOKEN_PERMISSIONS.map((permission) => (
                  <label
                    key={permission.value}
                    className="flex cursor-pointer items-start gap-3 rounded-md p-2 hover:bg-muted/50"
                  >
                    <Checkbox
                      checked={permissions.includes(permission.value)}
                      onCheckedChange={(checked) =>
                        togglePermission(permission.value, checked === true)
                      }
                    />
                    <span>
                      <span className="block text-sm font-medium">
                        {permission.label}
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {permission.description}
                      </span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            {createError && (
              <p role="alert" className="text-sm text-destructive">
                {createError}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setCreateOpen(false)}
              disabled={createMutation.isPending}
            >
              Cancel
            </Button>
            <Button onClick={submitCreate} disabled={createMutation.isPending}>
              {createMutation.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Create token
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete confirmation dialog */}
      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Delete token &quot;{deleteTarget?.name}&quot;?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently revoke the token. Any deployed application
              still using it will lose API access immediately. This action
              cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (!deleteTarget) return
                deleteMutation.mutate({
                  path: { project_id: project.id, token_id: deleteTarget.id },
                })
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {deleteMutation.isPending && (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              )}
              Delete Token
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Rotate confirmation dialog */}
      <AlertDialog
        open={rotateTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRotateTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Rotate token &quot;{rotateTarget?.name}&quot;?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This will invalidate the current token and issue a new one. Any
              deployed application using the old token will lose API access
              immediately. Make sure to update the token wherever it is used
              after rotating.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={rotateMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (!rotateTarget) return
                rotateMutation.mutate({
                  path: { project_id: project.id, token_id: rotateTarget.id },
                })
              }}
              disabled={rotateMutation.isPending}
            >
              {rotateMutation.isPending && (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              )}
              Rotate Token
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

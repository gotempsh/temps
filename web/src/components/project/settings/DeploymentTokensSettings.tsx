// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import {
  listDeploymentTokensOptions,
  listDeploymentTokensQueryKey,
  deleteDeploymentTokenMutation,
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, RefreshCw, Trash2, Key } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import type { DeploymentTokenResponse } from '@/api/client/types.gen'

interface DeploymentTokensSettingsProps {
  project: ProjectResponse
}

export function DeploymentTokensSettings({ project }: DeploymentTokensSettingsProps) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } = useSensitiveActionVerification()

  const [deleteTarget, setDeleteTarget] = useState<DeploymentTokenResponse | null>(null)
  const [rotateTarget, setRotateTarget] = useState<DeploymentTokenResponse | null>(null)
  // After a successful rotation the new plaintext token is stored here (shown once).
  const [revealedToken, setRevealedToken] = useState<{ id: number; value: string } | null>(null)

  const tokensQuery = useQuery({
    ...listDeploymentTokensOptions({
      path: { project_id: project.id },
      query: { page: 1, page_size: 100 },
    }),
  })

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
        handleSensitiveActionError(error, () => deleteMutation.mutate(variables))
      ) {
        setDeleteTarget(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(problem.detail || problem.message || 'Failed to delete deployment token')
    },
  })

  const rotateMutation = useMutation({
    ...rotateDeploymentTokenMutation(),
    onSuccess: (data) => {
      toast.success('Deployment token rotated — copy the new token now, it will not be shown again')
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
        handleSensitiveActionError(error, () => rotateMutation.mutate(variables))
      ) {
        setRotateTarget(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(problem.detail || problem.message || 'Failed to rotate deployment token')
    },
  })

  const tokens = tokensQuery.data?.tokens ?? []

  return (
    <div className="space-y-6">
      {verificationDialog}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Key className="h-5 w-5" />
            Deployment Tokens
          </CardTitle>
          <CardDescription>
            Deployment tokens provide <code>TEMPS_API_URL</code> and{' '}
            <code>TEMPS_API_TOKEN</code> credentials that are automatically
            injected into deployed applications.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {revealedToken && (
            <div className="mb-4 rounded-md border border-amber-300 bg-amber-50 p-4">
              <p className="text-sm font-medium text-amber-800 mb-2">
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
                className="mt-2 text-xs text-amber-700"
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
            <p className="text-sm text-destructive">Failed to load deployment tokens.</p>
          ) : tokens.length === 0 ? (
            <EmptyState
              icon={Key}
              title="No deployment tokens yet"
              description={
                <p className="text-sm text-muted-foreground">
                  Create one via the API using{' '}
                  <code>POST /projects/{'{project_id}'}/deployment-tokens</code>.
                </p>
              }
            />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="hidden md:table-cell">Prefix</TableHead>
                    <TableHead className="hidden md:table-cell">Status</TableHead>
                    <TableHead className="hidden md:table-cell">Last used</TableHead>
                    <TableHead className="text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {tokens.map((token) => (
                    <TableRow key={token.id}>
                      <TableCell className="font-medium">{token.name}</TableCell>
                      <TableCell className="hidden md:table-cell font-mono text-xs">
                        {token.token_prefix}…
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <Badge variant={token.is_active ? 'default' : 'secondary'}>
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
                            disabled={rotateMutation.isPending && rotateTarget?.id === token.id}
                          >
                            {rotateMutation.isPending && rotateTarget?.id === token.id ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <RefreshCw className="h-4 w-4" />
                            )}
                            <span className="hidden sm:inline ml-1">Rotate</span>
                          </Button>
                          <Button
                            size="sm"
                            variant="destructive"
                            onClick={() => setDeleteTarget(token)}
                            disabled={deleteMutation.isPending && deleteTarget?.id === token.id}
                          >
                            {deleteMutation.isPending && deleteTarget?.id === token.id ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <Trash2 className="h-4 w-4" />
                            )}
                            <span className="hidden sm:inline ml-1">Delete</span>
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

      {/* Delete confirmation dialog */}
      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => { if (!open) setDeleteTarget(null) }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete token &quot;{deleteTarget?.name}&quot;?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently revoke the token. Any deployed application
              still using it will lose API access immediately. This action cannot
              be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>Cancel</AlertDialogCancel>
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
        onOpenChange={(open) => { if (!open) setRotateTarget(null) }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Rotate token &quot;{rotateTarget?.name}&quot;?</AlertDialogTitle>
            <AlertDialogDescription>
              This will invalidate the current token and issue a new one. Any
              deployed application using the old token will lose API access
              immediately. Make sure to update the token wherever it is used
              after rotating.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={rotateMutation.isPending}>Cancel</AlertDialogCancel>
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

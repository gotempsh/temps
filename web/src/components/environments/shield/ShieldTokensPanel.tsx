// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createShieldTokenMutation,
  listShieldTokensOptions,
  listShieldTokensQueryKey,
  revokeShieldTokenMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ShieldTokenResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
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
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { KeyRound, Loader2, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { Controller, useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

const tokenSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Name the token after what will send it.')
    .max(128, 'At most 128 characters.'),
  project_wide: z.boolean(),
})

type TokenForm = z.infer<typeof tokenSchema>

export function ShieldTokensPanel({
  path,
  environmentName,
}: {
  path: ShieldEnvironmentPath
  environmentName: string
}) {
  const queryClient = useQueryClient()
  const [createOpen, setCreateOpen] = useState(false)
  // The minted token, shown once in the dialog and dropped when it closes.
  const [secret, setSecret] = useState<string | null>(null)
  const [revokeTarget, setRevokeTarget] = useState<ShieldTokenResponse | null>(
    null
  )

  const tokens = useQuery({ ...listShieldTokensOptions({ path }) })
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: listShieldTokensQueryKey({ path }),
    })

  const form = useForm<TokenForm>({
    resolver: zodResolver(tokenSchema),
    defaultValues: { name: '', project_wide: false },
  })

  const create = useMutation({
    ...createShieldTokenMutation(),
    meta: { errorTitle: 'Failed to create bypass token' },
    onSuccess: (created) => {
      setSecret(created.secret)
      form.reset({ name: '', project_wide: false })
      toast.success('Bypass token created. Copy it now; it is not shown again.')
      void invalidate()
    },
  })

  const revoke = useMutation({
    ...revokeShieldTokenMutation(),
    meta: { errorTitle: 'Failed to revoke bypass token' },
    onSuccess: () => {
      toast.success('Bypass token revoked')
      setRevokeTarget(null)
      void invalidate()
    },
  })

  const openCreate = () => {
    setSecret(null)
    form.reset({ name: '', project_wide: false })
    setCreateOpen(true)
  }

  const closeCreate = (open: boolean) => {
    if (create.isPending) return
    setCreateOpen(open)
    if (!open) setSecret(null)
  }

  const rows = tokens.data ?? []
  const createButton = (
    <Button type="button" size="sm" onClick={openCreate}>
      <Plus className="mr-2 h-4 w-4" />
      Create token
    </Button>
  )

  return (
    <ShieldSection
      id="shield-tokens"
      title="Bypass tokens"
      description="Monitors, CI and API clients that send a token in the X-Temps-Shield-Token header are never challenged."
      actions={rows.length > 0 ? createButton : undefined}
    >
      {tokens.error && (
        <div className="p-4">
          <ReadFailure
            resource="Bypass tokens"
            error={tokens.error}
            cached={tokens.data !== undefined}
            onRetry={() => tokens.refetch()}
            retrying={tokens.isFetching}
            embedded
          />
        </div>
      )}
      {tokens.data === undefined ? (
        tokens.error ? null : (
          <div
            role="status"
            aria-label="Loading bypass tokens"
            className="space-y-2 p-4"
          >
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        )
      ) : rows.length === 0 ? (
        <EmptyState
          size="compact"
          icon={KeyRound}
          title="No bypass tokens yet"
          description="Create one for each monitor or client that must reach this environment without a challenge."
          action={createButton}
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Scope</TableHead>
              <TableHead className="hidden md:table-cell">Created</TableHead>
              <TableHead className="hidden md:table-cell">Last used</TableHead>
              <TableHead className="text-right">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((token) => (
              <TableRow key={token.id}>
                <TableCell className="font-medium">{token.name}</TableCell>
                <TableCell>
                  <Badge variant="neutral-subtle">
                    {token.scope === 'project'
                      ? 'All environments'
                      : 'This environment'}
                  </Badge>
                </TableCell>
                <TableCell className="hidden text-sm text-muted-foreground md:table-cell">
                  <TimeAgo date={token.created_at} />
                </TableCell>
                <TableCell className="hidden text-sm text-muted-foreground md:table-cell">
                  {token.last_used_at ? (
                    <TimeAgo date={token.last_used_at} />
                  ) : (
                    'Never'
                  )}
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    aria-label={`Revoke ${token.name}`}
                    onClick={() => setRevokeTarget(token)}
                  >
                    <Trash2 className="h-4 w-4" />
                    <span className="ml-1 max-sm:sr-only">Revoke</span>
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}

      <Dialog open={createOpen} onOpenChange={closeCreate}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {secret ? 'Copy your bypass token' : 'Create bypass token'}
            </DialogTitle>
            <DialogDescription>
              {secret
                ? 'This is the only time the token is shown. Store it where the client that sends it can read it.'
                : `Requests that carry this token are never challenged in ${environmentName}, or in every environment of the service if you choose so.`}
            </DialogDescription>
          </DialogHeader>
          {secret ? (
            <div className="space-y-4">
              <div className="flex items-center gap-2 rounded-md border bg-muted/40 p-3">
                <code className="min-w-0 flex-1 break-all font-mono text-xs">
                  {secret}
                </code>
                <CopyButton value={secret} label="Copy bypass token" />
              </div>
              <p className="text-xs text-muted-foreground">
                Send it as <code>X-Temps-Shield-Token: &lt;token&gt;</code>.
              </p>
              <DialogFooter>
                <Button type="button" onClick={() => closeCreate(false)}>
                  I have saved the token
                </Button>
              </DialogFooter>
            </div>
          ) : (
            <form
              noValidate
              className="space-y-4"
              onSubmit={form.handleSubmit((values) =>
                create.mutate({ path, body: values })
              )}
            >
              <div className="space-y-2">
                <Label htmlFor="shield-token-name">Name</Label>
                <Input
                  id="shield-token-name"
                  placeholder="Uptime monitor"
                  autoComplete="off"
                  aria-invalid={form.formState.errors.name ? 'true' : undefined}
                  aria-describedby={
                    form.formState.errors.name
                      ? 'shield-token-name-error'
                      : undefined
                  }
                  {...form.register('name')}
                />
                {form.formState.errors.name && (
                  <p
                    id="shield-token-name-error"
                    className="text-sm text-destructive"
                  >
                    {form.formState.errors.name.message}
                  </p>
                )}
              </div>
              <Controller
                control={form.control}
                name="project_wide"
                render={({ field }) => (
                  <div className="flex items-start gap-3">
                    <Checkbox
                      id="shield-token-project-wide"
                      checked={field.value}
                      onCheckedChange={(value) =>
                        field.onChange(value === true)
                      }
                    />
                    <div className="space-y-0.5">
                      <Label htmlFor="shield-token-project-wide">
                        Valid in every environment
                      </Label>
                      <p className="text-xs text-muted-foreground">
                        Otherwise the token only works in {environmentName}.
                      </p>
                    </div>
                  </div>
                )}
              />
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => closeCreate(false)}
                  disabled={create.isPending}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={create.isPending}>
                  {create.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Create token
                </Button>
              </DialogFooter>
            </form>
          )}
        </DialogContent>
      </Dialog>

      <ConfirmDeleteDialog
        open={revokeTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRevokeTarget(null)
        }}
        resourceType="bypass token"
        resourceName={revokeTarget?.name ?? ''}
        verb="Revoke"
        consequences="Clients sending this token are treated like any other visitor again, within seconds."
        isPending={revoke.isPending}
        onConfirm={() => {
          if (!revokeTarget) return
          revoke.mutate({ path: { ...path, token_id: revokeTarget.id } })
        }}
      />
    </ShieldSection>
  )
}

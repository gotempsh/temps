// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { Link } from 'react-router'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, ArrowUpRight, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import {
  getSandboxPlacementOptions,
  getSandboxPlacementQueryKey,
  updateSandboxPlacementMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { PlacementNode } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
} from '@/components/ui/form'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { useAuth } from '@/contexts/AuthContext-shared'
import { problemDetail } from '@/lib/api-problem'
import { canManageSandboxPlacement } from './helpers'
import {
  allowedNodeIdsFromForm,
  placementExcludesControlPlane,
  placementFormDirty,
  placementFormValues,
  sandboxPlacementFormSchema,
  sortedIds,
  type SandboxPlacementFormValues,
} from './sandbox-placement'

/**
 * Which nodes may run sandboxes (ADR-048). `null` = every node, the
 * default. The control plane is node `0`. Existing sandboxes are never
 * moved or stopped by this setting — it only governs new placements.
 */
export function SandboxNodesCard() {
  const queryClient = useQueryClient()
  const { user } = useAuth()
  const canEdit = canManageSandboxPlacement(user?.role)
  const placement = useQuery(getSandboxPlacementOptions())

  const form = useForm<SandboxPlacementFormValues>({
    resolver: zodResolver(sandboxPlacementFormSchema),
    defaultValues: placementFormValues(placement.data),
  })
  // A background refetch must not throw away a selection being edited.
  useEffect(() => {
    if (!placement.data) return
    form.reset(placementFormValues(placement.data), { keepDirtyValues: true })
  }, [placement.data, form])

  const values = useWatch({ control: form.control }) as SandboxPlacementFormValues
  const current: SandboxPlacementFormValues = {
    allowAll: values.allowAll ?? true,
    selected: values.selected ?? [],
  }
  const dirty = placementFormDirty(placement.data, current)
  const excludesControlPlane = placementExcludesControlPlane(current)

  const save = useMutation({
    ...updateSandboxPlacementMutation(),
    meta: { errorTitle: 'Failed to update sandbox nodes' },
    onSuccess: (data) => {
      queryClient.setQueryData(getSandboxPlacementQueryKey(), data)
      form.reset(placementFormValues(data))
      // Node pages show whether each node accepts new sandboxes.
      void queryClient.invalidateQueries({
        queryKey: getSandboxPlacementQueryKey(),
      })
      void queryClient.invalidateQueries({
        queryKey: [{ _id: 'listNodeSandboxes' }],
      })
      toast.success('Sandbox nodes updated', {
        description: 'Existing sandboxes keep running where they are.',
      })
    },
  })

  const onSubmit = (submitted: SandboxPlacementFormValues) =>
    save.mutate({
      // Always explicit: `null` means every node, and the server requires
      // the member so an empty body can never reset the allow-list.
      body: { allowed_node_ids: allowedNodeIdsFromForm(submitted) },
    })

  const nodes: PlacementNode[] = placement.data?.nodes ?? []
  const onlyControlPlane = nodes.length === 1

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Sandbox nodes</CardTitle>
        <CardDescription>
          Choose which nodes can run new sandboxes. When a sandbox is created
          without a node, Temps uses the control plane if it is allowed,
          otherwise the allowed worker with the fewest sandboxes. Sandboxes that
          already exist keep running where they are.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {placement.isError && (
          <Alert variant="destructive">
            <AlertDescription className="flex items-center justify-between gap-2">
              <span>
                Could not load sandbox nodes:{' '}
                {problemDetail(placement.error, 'Try again in a moment.')}
              </span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => placement.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        )}
        {placement.isLoading && <SandboxNodesSkeleton />}
        {placement.data && (
          <Form {...form}>
            <form
              className="space-y-4"
              onSubmit={form.handleSubmit(onSubmit)}
            >
              <FormField
                control={form.control}
                name="allowAll"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center gap-2 space-y-0">
                    <FormControl>
                      <Checkbox
                        checked={field.value}
                        disabled={!canEdit}
                        onCheckedChange={(v) => {
                          field.onChange(v === true)
                          // Turning "every node" off starts from every node
                          // rather than from an empty list.
                          if (v !== true && current.selected.length === 0) {
                            form.setValue(
                              'selected',
                              sortedIds(nodes.map((n) => n.id))
                            )
                          }
                        }}
                      />
                    </FormControl>
                    <FormLabel className="text-sm font-normal">
                      Allow every node, including nodes added later
                    </FormLabel>
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="selected"
                render={({ field }) => (
                  <div className="divide-y rounded-md border">
                    {nodes.map((node) => (
                      <SandboxNodeRow
                        key={node.id}
                        node={node}
                        checked={
                          current.allowAll || field.value.includes(node.id)
                        }
                        disabled={current.allowAll || !canEdit}
                        onCheckedChange={(checked) =>
                          field.onChange(
                            checked
                              ? sortedIds([...field.value, node.id])
                              : field.value.filter((id) => id !== node.id)
                          )
                        }
                      />
                    ))}
                  </div>
                )}
              />
              {!current.allowAll && current.selected.length === 0 && (
                <p className="text-xs text-destructive">
                  With no node selected, nobody can create new sandboxes.
                </p>
              )}
              {excludesControlPlane && <ControlPlaneExcludedWarning />}
              {onlyControlPlane && (
                <p className="text-xs text-muted-foreground">
                  Only the control plane is available. Run{' '}
                  <code className="font-mono">temps join</code> on another
                  machine to add a worker, then run sandboxes there with{' '}
                  <code className="font-mono">
                    bunx @temps-sdk/cli sandbox create --node &lt;worker&gt;
                  </code>
                  . See{' '}
                  <Link
                    to="/settings/nodes"
                    className="underline underline-offset-2"
                  >
                    Nodes
                  </Link>
                  .
                </p>
              )}
              <div className="flex items-center justify-end gap-3">
                {!canEdit && (
                  <span className="text-xs text-muted-foreground">
                    Only administrators can change which nodes run sandboxes.
                  </span>
                )}
                <Button
                  type="submit"
                  size="sm"
                  disabled={!canEdit || !dirty || save.isPending}
                >
                  {save.isPending && (
                    <Loader2 className="mr-1 h-4 w-4 animate-spin" />
                  )}
                  {save.isPending ? 'Saving…' : 'Save'}
                </Button>
              </div>
            </form>
          </Form>
        )}
      </CardContent>
    </Card>
  )
}

function SandboxNodeRow({
  node,
  checked,
  disabled,
  onCheckedChange,
}: {
  node: PlacementNode
  checked: boolean
  disabled: boolean
  onCheckedChange: (checked: boolean) => void
}) {
  const label = node.is_control_plane ? 'Control plane' : node.name
  return (
    <div className="flex items-center justify-between gap-3 px-3 py-2">
      <div className="min-w-0 space-y-0.5">
        <div className="flex min-w-0 items-center gap-2">
          <Checkbox
            id={`sandbox-node-${node.id}`}
            checked={checked}
            disabled={disabled}
            onCheckedChange={(v) => onCheckedChange(v === true)}
          />
          <Label
            htmlFor={`sandbox-node-${node.id}`}
            className="truncate text-sm font-medium"
          >
            {label}
          </Label>
          {!node.is_control_plane && (
            <Link
              to={`/settings/nodes/${node.id}`}
              title={`Open node ${node.name}`}
              aria-label={`Open node ${node.name}`}
              className="text-muted-foreground hover:text-foreground"
            >
              <ArrowUpRight className="h-3.5 w-3.5" />
            </Link>
          )}
          <Badge variant={node.status === 'active' ? 'outline' : 'secondary'}>
            {node.status}
          </Badge>
        </div>
        {/* Why an allowed node still won't take sandboxes (offline, being
            evicted, plain-http address, …). */}
        {!node.eligible && node.reason && (
          <p className="pl-6 text-xs text-muted-foreground">
            Not taking new sandboxes: {node.reason}
          </p>
        )}
      </div>
      <span className="shrink-0 text-xs text-muted-foreground">
        {node.live_sandboxes} sandbox
        {node.live_sandboxes === 1 ? '' : 'es'}
      </span>
    </div>
  )
}

/** Shown while the selection leaves the control plane out. */
export function ControlPlaneExcludedWarning() {
  return (
    <Alert>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>The control plane will not take new sandboxes</AlertTitle>
      <AlertDescription className="space-y-1 text-xs">
        <p>
          Sandboxes created without a node will go to the allowed worker with
          the fewest sandboxes. On worker nodes the terminal, the agent runtime
          (Fleet), snapshots, preview URLs, disk resize and volumes are not
          available yet.
        </p>
        <p>
          Managed AI application workspaces still always run on the control
          plane, whatever this list says.
        </p>
      </AlertDescription>
    </Alert>
  )
}

function SandboxNodesSkeleton() {
  return (
    <div
      className="space-y-4"
      aria-busy="true"
      aria-label="Loading sandbox nodes"
    >
      <div className="flex items-center gap-2">
        <Skeleton className="h-4 w-4" />
        <Skeleton className="h-4 w-64" />
      </div>
      <div className="divide-y rounded-md border">
        {[0, 1].map((row) => (
          <div
            key={row}
            data-testid="sandbox-node-skeleton-row"
            className="flex items-center justify-between gap-3 px-3 py-2"
          >
            <div className="flex items-center gap-2">
              <Skeleton className="h-4 w-4" />
              <Skeleton className="h-4 w-28" />
              <Skeleton className="h-5 w-14" />
            </div>
            <Skeleton className="h-3 w-20" />
          </div>
        ))}
      </div>
      <div className="flex justify-end">
        <Skeleton className="h-8 w-16" />
      </div>
    </div>
  )
}

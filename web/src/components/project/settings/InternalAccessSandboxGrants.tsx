// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Sandbox callers on the Internal access card (ADR 105): which of the
 * user's sandboxes may call this project's `*.temps.local` addresses. A
 * sandbox calls them with plain `http://` through its egress proxy, in
 * every egress mode; without a grant it gets a 403 naming this card.
 */
import {
  createInternalAccessSandboxGrant,
  deleteInternalAccessSandboxGrant,
  listInternalAccessSandboxGrants,
} from '@/api/client'
import { listInternalAccessSandboxGrantsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  EnvironmentResponse,
  InternalAccessSandboxGrantListResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'
import { SandboxSelect } from '@/components/sandboxes/SandboxSelect'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Box, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  clampGrantsPage,
  GRANTS_PAGE_SIZE,
  MAX_GRANT_DESCRIPTION_LENGTH,
  emptySandboxGrantForm,
  isSandboxGrantInherited,
  sandboxCallerLabel,
  sandboxCurl,
  sandboxGrantFormSchema,
  sandboxTargetLabel,
  toCreateSandboxRequest,
  type SandboxGrantFormValues,
} from './internal-access'

const ALL = 'all'

export function InternalAccessSandboxGrants({
  projectId,
  projectName,
  projectSlug,
  environmentId,
  environmentName,
  environmentSlug,
  ownEnvironments,
}: {
  projectId: number
  projectName: string
  projectSlug: string
  environmentId?: number
  environmentName?: string
  environmentSlug?: string
  /** This project's environments, for the project card's target choice. */
  ownEnvironments: EnvironmentResponse[] | undefined
}) {
  const queryClient = useQueryClient()
  const isEnvironment = environmentId !== undefined
  const [page, setPage] = useState(1)
  const [removing, setRemoving] =
    useState<InternalAccessSandboxGrantResponse | null>(null)

  const listOptions = {
    path: { project_id: projectId },
    query: { environment_id: environmentId, page, page_size: GRANTS_PAGE_SIZE },
  }
  const grantsQuery = useQuery({
    queryKey: listInternalAccessSandboxGrantsQueryKey(listOptions),
    queryFn: async (): Promise<InternalAccessSandboxGrantListResponse> => {
      const result = await listInternalAccessSandboxGrants({
        ...listOptions,
        throwOnError: true,
      })
      return result.data
    },
  })
  const invalidate = () =>
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as
          { _id?: string; path?: { project_id?: number } } | undefined
        return (
          key?._id === 'listInternalAccessSandboxGrants' &&
          key.path?.project_id === projectId
        )
      },
    })

  const form = useForm<SandboxGrantFormValues>({
    resolver: zodResolver(sandboxGrantFormSchema),
    defaultValues: emptySandboxGrantForm(environmentId),
  })

  const create = useMutation({
    mutationFn: async (values: SandboxGrantFormValues) => {
      const body = toCreateSandboxRequest(values)
      if (body === null) throw new Error('Choose the sandbox first')
      const result = await createInternalAccessSandboxGrant({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      })
      return result.data
    },
    onSuccess: (grant) => {
      form.reset(emptySandboxGrantForm(environmentId))
      setPage(1)
      invalidate()
      toast.success(`${sandboxCallerLabel(grant)} can now call ${projectName}`)
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Failed to allow the sandbox'))
    },
  })

  const remove = useMutation({
    mutationFn: async (grant: InternalAccessSandboxGrantResponse) => {
      await deleteInternalAccessSandboxGrant({
        path: { project_id: projectId, grant_id: grant.id },
        throwOnError: true,
      })
      return grant
    },
    onSuccess: (grant) => {
      setRemoving(null)
      const listed = grantsQuery.data
      if (listed) {
        setPage(clampGrantsPage(page, listed.total - 1, listed.page_size))
      }
      invalidate()
      toast.success(
        `${sandboxCallerLabel(grant)} can no longer call ${projectName}`
      )
    },
    onError: (error) => {
      setRemoving(null)
      toast.error(problemDetail(error, 'Failed to remove the sandbox'))
    },
  })

  const grants = grantsQuery.data
  const totalPages = grants ? Math.ceil(grants.total / grants.page_size) : 0
  const curl = sandboxCurl(projectSlug, environmentSlug)

  return (
    <section className="space-y-3" aria-label="Sandbox callers">
      <h3 className="flex items-center gap-2 text-sm font-medium">
        <Box className="h-4 w-4" aria-hidden />
        Sandboxes
      </h3>
      {grantsQuery.isError ? (
        <ErrorAlert
          title="Could not load sandbox callers"
          description={problemDetail(
            grantsQuery.error,
            'The sandbox grants could not be loaded.'
          )}
          retry={() => grantsQuery.refetch()}
          retrying={grantsQuery.isFetching}
        />
      ) : grants === undefined ? (
        <div className="space-y-2" aria-busy="true">
          <Skeleton className="h-12 w-full" />
        </div>
      ) : grants.total === 0 ? (
        <div className="rounded-md border border-dashed p-4 text-sm">
          <p className="font-medium">No sandbox may call it.</p>
          <p className="mt-1 text-muted-foreground [overflow-wrap:anywhere]">
            A sandbox running <code className="text-xs">{curl}</code> gets a
            403. Add one of your sandboxes below; its page then lists this
            address.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          <ul
            className="divide-y rounded-md border"
            aria-label="Sandbox grants"
          >
            {grants.items.map((grant) => (
              <SandboxGrantRow
                key={grant.id}
                grant={grant}
                inherited={isSandboxGrantInherited(grant, environmentId)}
                onRemove={() => setRemoving(grant)}
              />
            ))}
          </ul>
          {totalPages > 1 && (
            <ResponsivePagination
              page={page}
              pageSize={grants.page_size}
              total={grants.total}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Sandbox caller pages"
            />
          )}
        </div>
      )}

      <Form {...form}>
        <form
          onSubmit={form.handleSubmit((values) => create.mutate(values))}
          className="space-y-4 rounded-md border p-4"
          noValidate
          aria-label="Allow a sandbox"
        >
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <FormField
              control={form.control}
              name="sandboxId"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Calling sandbox</FormLabel>
                  <SandboxSelect
                    ariaLabel="Calling sandbox"
                    value={field.value === '' ? null : field.value}
                    onValueChange={(next) => field.onChange(next ?? '')}
                    className="w-full"
                  />
                  <FormMessage />
                </FormItem>
              )}
            />
            {!isEnvironment && (
              <FormField
                control={form.control}
                name="targetEnvironmentId"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>May call</FormLabel>
                    <Select
                      value={field.value === null ? ALL : String(field.value)}
                      onValueChange={(value) =>
                        field.onChange(value === ALL ? null : Number(value))
                      }
                    >
                      <FormControl>
                        <SelectTrigger
                          className="w-full"
                          aria-label="Sandbox may call"
                        >
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value={ALL}>
                          Every environment of {projectName}
                        </SelectItem>
                        {ownEnvironments?.map((environment) => (
                          <SelectItem
                            key={environment.id}
                            value={String(environment.id)}
                          >
                            {environment.name} only
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}
            <FormField
              control={form.control}
              name="description"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Reason (optional)</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      maxLength={MAX_GRANT_DESCRIPTION_LENGTH}
                      placeholder="e.g. the agent tests the orders API"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit" disabled={create.isPending}>
              {create.isPending
                ? 'Allowing…'
                : `Allow the sandbox to call ${
                    isEnvironment
                      ? `${projectName} ${environmentName ?? 'this environment'}`
                      : projectName
                  }`}
            </Button>
            <Link
              to="/sandboxes"
              className="text-sm text-muted-foreground underline underline-offset-4"
            >
              Your sandboxes
            </Link>
          </div>
        </form>
      </Form>

      <ConfirmDeleteDialog
        open={removing !== null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null)
        }}
        resourceType="sandbox access"
        resourceName={removing ? sandboxCallerLabel(removing) : ''}
        verb="Remove"
        irreversible={false}
        consequences={`Calls from it to ${projectName} get 403 again within seconds.`}
        isPending={remove.isPending}
        onConfirm={() => {
          if (removing) remove.mutate(removing)
        }}
      />
    </section>
  )
}

function SandboxGrantRow({
  grant,
  inherited,
  onRemove,
}: {
  grant: InternalAccessSandboxGrantResponse
  inherited: boolean
  onRemove: () => void
}) {
  return (
    <li className="flex items-start justify-between gap-3 p-3">
      <div className="min-w-0 space-y-1">
        <p className="text-sm font-medium [overflow-wrap:anywhere]">
          {sandboxCallerLabel(grant)}
        </p>
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <span>May call: {sandboxTargetLabel(grant)}</span>
          {inherited && <Badge variant="secondary">Project-wide</Badge>}
        </div>
        {grant.description && (
          <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
            {grant.description}
          </p>
        )}
      </div>
      {!inherited && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={onRemove}
          aria-label={`Remove internal access for ${sandboxCallerLabel(grant)}`}
        >
          <Trash2 className="h-4 w-4" />
          <span className="max-sm:sr-only">Remove</span>
        </Button>
      )}
    </li>
  )
}

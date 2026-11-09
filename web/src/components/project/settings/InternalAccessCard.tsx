// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Internal access (ADR 101): which other projects may call this project's
 * `*.temps.local` addresses through the internal proxy.
 *
 * By default only the project's own environments can call each other; every
 * other project gets a 403. A grant here lets one other project, or one
 * environment of it, call this whole project or one environment of it. The
 * project card manages every grant; an environment's card manages the grants
 * on that environment and shows the project-wide ones as inherited.
 */
import {
  createInternalAccessGrant,
  deleteInternalAccessGrant,
  getEnvironments,
  listInternalAccessGrants,
} from '@/api/client'
import { listInternalAccessGrantsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  EnvironmentResponse,
  InternalAccessGrantListResponse,
  InternalAccessGrantResponse,
} from '@/api/client/types.gen'
import { ProjectSelect } from '@/components/project/ProjectSelect'
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
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Trash2, Waypoints } from 'lucide-react'
import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import {
  GRANTS_PAGE_SIZE,
  INTERNAL_ACCESS_SECTION_ID,
  MAX_GRANT_DESCRIPTION_LENGTH,
  callerLabel,
  emptyGrantForm,
  allowLabel,
  exampleAddress,
  internalAccessFormSchema,
  isInherited,
  targetLabel,
  toCreateRequest,
  type InternalAccessFormValues,
} from './internal-access'

interface InternalAccessCardProps {
  projectId: number
  projectName: string
  projectSlug: string
  /** Set to manage the grants on one environment instead of the project. */
  environmentId?: number
  environmentName?: string
  environmentSlug?: string
}

/** The `Select` value for "every environment". */
const ALL = 'all'

function environmentValue(id: number | null): string {
  return id === null ? ALL : String(id)
}

function parseEnvironmentValue(value: string): number | null {
  return value === ALL ? null : Number(value)
}

async function fetchEnvironments(
  projectId: number
): Promise<EnvironmentResponse[]> {
  const result = await getEnvironments({
    path: { project_id: projectId },
    throwOnError: true,
  })
  return result.data
}

export function InternalAccessCard({
  projectId,
  projectName,
  projectSlug,
  environmentId,
  environmentName,
  environmentSlug,
}: InternalAccessCardProps) {
  const queryClient = useQueryClient()
  const isEnvironment = environmentId !== undefined
  const [page, setPage] = useState(1)
  const [removing, setRemoving] = useState<InternalAccessGrantResponse | null>(
    null
  )

  const listOptions = {
    path: { project_id: projectId },
    query: {
      environment_id: environmentId,
      page,
      page_size: GRANTS_PAGE_SIZE,
    },
  }
  const grantsQuery = useQuery({
    queryKey: listInternalAccessGrantsQueryKey(listOptions),
    queryFn: async (): Promise<InternalAccessGrantListResponse> => {
      const result = await listInternalAccessGrants({
        ...listOptions,
        throwOnError: true,
      })
      return result.data
    },
  })
  // Every page of this project's grants, whatever the environment filter.
  const invalidateGrants = () =>
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as
          { _id?: string; path?: { project_id?: number } } | undefined
        return (
          key?._id === 'listInternalAccessGrants' &&
          key.path?.project_id === projectId
        )
      },
    })

  const form = useForm<InternalAccessFormValues>({
    resolver: zodResolver(internalAccessFormSchema),
    defaultValues: emptyGrantForm(environmentId),
  })
  const callerProjectId = useWatch({
    control: form.control,
    name: 'callerProjectId',
  })

  const ownEnvironments = useQuery({
    queryKey: ['internal-access', 'environments', projectId],
    queryFn: () => fetchEnvironments(projectId),
    enabled: !isEnvironment,
  })
  const callerEnvironments = useQuery({
    queryKey: ['internal-access', 'environments', callerProjectId],
    queryFn: () => fetchEnvironments(callerProjectId ?? 0),
    enabled: callerProjectId !== null,
  })

  const create = useMutation({
    mutationFn: async (values: InternalAccessFormValues) => {
      const body = toCreateRequest(values)
      if (body === null) throw new Error('Choose the calling project first')
      const result = await createInternalAccessGrant({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      })
      return result.data
    },
    onSuccess: (grant) => {
      form.reset(emptyGrantForm(environmentId))
      setPage(1)
      invalidateGrants()
      toast.success(
        `${callerLabel(grant)} can now call ${
          grant.target_environment_id == null
            ? projectName
            : `${projectName} ${grant.target_environment_name ?? ''}`.trim()
        }`
      )
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Failed to allow internal access'))
    },
  })

  const remove = useMutation({
    mutationFn: async (grant: InternalAccessGrantResponse) => {
      await deleteInternalAccessGrant({
        path: { project_id: projectId, grant_id: grant.id },
        throwOnError: true,
      })
      return grant
    },
    onSuccess: (grant) => {
      setRemoving(null)
      invalidateGrants()
      toast.success(`${callerLabel(grant)} can no longer call ${projectName}`)
    },
    onError: (error) => {
      setRemoving(null)
      toast.error(problemDetail(error, 'Failed to remove internal access'))
    },
  })

  const title = isEnvironment
    ? 'Internal access to this environment'
    : 'Internal access'
  const description = isEnvironment
    ? `Other projects that may call ${exampleAddress(projectSlug, environmentSlug)}.`
    : `Other projects that may call this project's *.temps.local addresses.`

  const onSubmit = form.handleSubmit((values) => create.mutate(values))
  const grants = grantsQuery.data
  const totalPages = grants ? Math.ceil(grants.total / grants.page_size) : 0
  const selectedCaller = callerEnvironments.data

  return (
    <SettingsSection
      id={isEnvironment ? undefined : INTERNAL_ACCESS_SECTION_ID}
      title={title}
      description={description}
      icon={Waypoints}
    >
      <div className="space-y-5">
        {grantsQuery.isError ? (
          <ErrorAlert
            title="Could not load internal access"
            description={problemDetail(
              grantsQuery.error,
              'The internal access grants could not be loaded.'
            )}
            retry={() => grantsQuery.refetch()}
            retrying={grantsQuery.isFetching}
          />
        ) : grants === undefined ? (
          <div className="space-y-2" aria-busy="true">
            <Skeleton className="h-5 w-64" />
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-full" />
          </div>
        ) : grants.total === 0 ? (
          <div className="rounded-md border border-dashed p-4 text-sm">
            <p className="font-medium">
              Only {projectName}&apos;s own environments can call it.
            </p>
            <p className="mt-1 text-muted-foreground [overflow-wrap:anywhere]">
              Another project calling{' '}
              <code className="text-xs">
                {exampleAddress(projectSlug, environmentSlug)}
              </code>{' '}
              gets a 403. Allow it below.
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            <ul className="divide-y rounded-md border" aria-label="Grants">
              {grants.items.map((grant) => (
                <GrantRow
                  key={grant.id}
                  grant={grant}
                  inherited={isInherited(grant, environmentId)}
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
                ariaLabel="Internal access pages"
              />
            )}
          </div>
        )}

        <Form {...form}>
          <form
            onSubmit={onSubmit}
            className="space-y-4 rounded-md border p-4"
            noValidate
            aria-label="Allow a project"
          >
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <FormField
                control={form.control}
                name="callerProjectId"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Calling project</FormLabel>
                    <ProjectSelect
                      ariaLabel="Calling project"
                      value={field.value}
                      onValueChange={(next) => {
                        field.onChange(next)
                        // Another project's environments are not this one's.
                        form.setValue('callerEnvironmentId', null)
                      }}
                      allowAll={false}
                      excludeIds={[projectId]}
                      placeholder="Select service…"
                      className="w-full"
                    />
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="callerEnvironmentId"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>From</FormLabel>
                    <Select
                      value={environmentValue(field.value)}
                      onValueChange={(value) =>
                        field.onChange(parseEnvironmentValue(value))
                      }
                      disabled={callerProjectId === null}
                    >
                      <FormControl>
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value={ALL}>
                          All environments, including new ones
                        </SelectItem>
                        {selectedCaller?.map((environment) => (
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
              {!isEnvironment && (
                <FormField
                  control={form.control}
                  name="targetEnvironmentId"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>May call</FormLabel>
                      <Select
                        value={environmentValue(field.value)}
                        onValueChange={(value) =>
                          field.onChange(parseEnvironmentValue(value))
                        }
                      >
                        <FormControl>
                          <SelectTrigger className="w-full">
                            <SelectValue />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          <SelectItem value={ALL}>
                            Every environment of {projectName}
                          </SelectItem>
                          {ownEnvironments.data?.map((environment) => (
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
                        placeholder="e.g. the worker calls the orders API"
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending
                ? 'Allowing…'
                : allowLabel(
                    isEnvironment
                      ? `${projectName} ${environmentName ?? 'this environment'}`
                      : projectName
                  )}
            </Button>
          </form>
        </Form>
      </div>

      <ConfirmDeleteDialog
        open={removing !== null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null)
        }}
        resourceType="internal access"
        resourceName={removing ? callerLabel(removing) : ''}
        verb="Remove"
        irreversible={false}
        consequences={`Calls from it to ${projectName} get 403 again within seconds.`}
        isPending={remove.isPending}
        onConfirm={() => {
          if (removing) remove.mutate(removing)
        }}
      />
    </SettingsSection>
  )
}

function GrantRow({
  grant,
  inherited,
  onRemove,
}: {
  grant: InternalAccessGrantResponse
  inherited: boolean
  onRemove: () => void
}) {
  return (
    <li className="flex items-start justify-between gap-3 p-3">
      <div className="min-w-0 space-y-1">
        <p className="text-sm font-medium [overflow-wrap:anywhere]">
          {callerLabel(grant)}
        </p>
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <span>May call: {targetLabel(grant)}</span>
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
          aria-label={`Remove internal access for ${callerLabel(grant)}`}
        >
          <Trash2 className="h-4 w-4" />
          <span className="max-sm:sr-only">Remove</span>
        </Button>
      )}
    </li>
  )
}

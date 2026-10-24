// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryResult,
} from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { HardDrive, Link2, Loader2, Plus, Trash2, Unlink } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { problemDetail } from '@/lib/api-problem'
import {
  WORKLOAD_PLANE_SECTION_ID,
  attachEnvironmentVolume,
  attachVolumeFormSchema,
  createEnvironmentVolume,
  createVolumeFormSchema,
  deleteEnvironmentVolume,
  detachEnvironmentVolume,
  environmentVolumeListQueryKey,
  environmentVolumeStatusQueryKey,
  environmentVolumesQueryKey,
  getEnvironmentVolumeStatus,
  isPermissionDenied,
  listEnvironmentVolumes,
  placementExplanationQueryKey,
  toCreateVolumeRequest,
  volumeDeleteConsequence,
  volumeDeletedMessage,
  volumeLocation,
  type AttachVolumeFormValues,
  type CreateVolumeFormValues,
  type EnvironmentVolume,
  type EnvironmentVolumeStatus,
} from '@/lib/environment-workload'

const PAGE_SIZE = 20
const TITLE = 'Volumes'
const DESCRIPTION =
  'Keep data such as uploads or a SQLite file across deployments by mounting a volume into every replica.'

/** Open the workload-plane section on this page and scroll to it. */
function revealWorkloadPlane() {
  const section = document.getElementById(WORKLOAD_PLANE_SECTION_ID)
  if (!section) return
  if (section instanceof HTMLDetailsElement) section.open = true
  section.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

/**
 * An environment's volumes (ADR-055 §12): create, attach at a mount path,
 * detach and delete, and whether the next deployment mounts them.
 * Instance-admin only, like the API behind it.
 */
export function EnvironmentVolumesCard({
  environmentId,
}: {
  environmentId: number
}) {
  const isAdmin = useIsInstanceAdmin()
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)
  const [createOpen, setCreateOpen] = useState(false)
  const [attachTarget, setAttachTarget] = useState<EnvironmentVolume | null>(
    null
  )
  const [detachTarget, setDetachTarget] = useState<EnvironmentVolume | null>(
    null
  )
  const [deleteTarget, setDeleteTarget] = useState<EnvironmentVolume | null>(
    null
  )
  const [writeDenied, setWriteDenied] = useState<string | null>(null)

  const listQuery = useQuery({
    queryKey: environmentVolumeListQueryKey(environmentId, page, PAGE_SIZE),
    queryFn: () => listEnvironmentVolumes(environmentId, page, PAGE_SIZE),
    enabled: isAdmin,
    placeholderData: keepPreviousData,
  })
  const statusQuery = useQuery({
    queryKey: environmentVolumeStatusQueryKey(environmentId),
    queryFn: () => getEnvironmentVolumeStatus(environmentId),
    enabled: isAdmin,
  })

  const createForm = useForm<CreateVolumeFormValues>({
    resolver: zodResolver(createVolumeFormSchema),
    defaultValues: { name: '', sizeMb: '', retainOnDelete: true },
  })
  const attachForm = useForm<AttachVolumeFormValues>({
    resolver: zodResolver(attachVolumeFormSchema),
    defaultValues: { mountPath: '/data', readOnly: false },
  })

  const refresh = () => {
    void queryClient.invalidateQueries({
      queryKey: environmentVolumesQueryKey(environmentId),
    })
    // An attached volume pins the environment; the explanation shows it.
    void queryClient.invalidateQueries({
      queryKey: placementExplanationQueryKey(environmentId),
    })
  }

  const onWriteError = (title: string) => (error: unknown) => {
    const detail = problemDetail(error, `${title}.`)
    if (isPermissionDenied(error)) setWriteDenied(detail)
    toast.error(title, { description: detail })
  }

  const create = useMutation({
    mutationFn: (values: CreateVolumeFormValues) =>
      createEnvironmentVolume(environmentId, toCreateVolumeRequest(values)),
    onSuccess: (volume) => {
      refresh()
      setCreateOpen(false)
      createForm.reset()
      toast.success(`Created volume ${volume.name}`, {
        description:
          'Attach it at a mount path to use it in the next deployment.',
      })
    },
    onError: onWriteError('Could not create the volume'),
  })

  const attach = useMutation({
    mutationFn: ({
      volume,
      values,
    }: {
      volume: EnvironmentVolume
      values: AttachVolumeFormValues
    }) =>
      attachEnvironmentVolume(environmentId, volume.id, {
        mount_path: values.mountPath,
        read_only: values.readOnly,
      }),
    onSuccess: (volume) => {
      refresh()
      setAttachTarget(null)
      toast.success(
        `Attached ${volume.name} at ${volume.attachment?.mount_path ?? 'its mount path'}`,
        { description: 'The next deployment mounts it.' }
      )
    },
    onError: onWriteError('Could not attach the volume'),
  })

  const detach = useMutation({
    mutationFn: (volume: EnvironmentVolume) =>
      detachEnvironmentVolume(environmentId, volume.id),
    onSuccess: (volume) => {
      refresh()
      setDetachTarget(null)
      toast.success(`Detached ${volume.name}`, {
        description:
          'Its data is kept; the next deployment no longer mounts it.',
      })
    },
    onError: (error) => {
      setDetachTarget(null)
      onWriteError('Could not detach the volume')(error)
    },
  })

  const remove = useMutation({
    mutationFn: (volume: EnvironmentVolume) =>
      deleteEnvironmentVolume(environmentId, volume.id),
    onSuccess: (result) => {
      if (listQuery.data?.volumes.length === 1 && page > 1) setPage(page - 1)
      refresh()
      setDeleteTarget(null)
      toast.success(volumeDeletedMessage(result))
    },
    onError: (error) => {
      setDeleteTarget(null)
      onWriteError('Could not delete the volume')(error)
    },
  })

  const openAttach = (volume: EnvironmentVolume) => {
    attachForm.reset({
      mountPath: volume.attachment?.mount_path ?? '/data',
      readOnly: volume.attachment?.read_only ?? false,
    })
    setAttachTarget(volume)
  }

  if (!isAdmin) {
    return (
      <Section>
        <AdminOnlyNotice />
      </Section>
    )
  }

  if (listQuery.isPending) {
    return (
      <Section>
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-14 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      </Section>
    )
  }

  if (listQuery.isError) {
    return (
      <Section>
        {isPermissionDenied(listQuery.error) ? (
          <AdminOnlyNotice
            detail={problemDetail(listQuery.error, 'Permission denied.')}
          />
        ) : (
          <ErrorAlert
            title="Could not load volumes"
            description={problemDetail(
              listQuery.error,
              'The volumes of this environment could not be loaded.'
            )}
            retry={() => listQuery.refetch()}
            retrying={listQuery.isFetching}
          />
        )}
      </Section>
    )
  }

  const list = listQuery.data
  const readOnly = writeDenied !== null
  const totalPages = Math.max(1, Math.ceil(list.total / list.page_size))

  return (
    <Section>
      <div className="space-y-4">
        {readOnly ? (
          <Callout tone="warning" title="Read-only">
            {writeDenied}
          </Callout>
        ) : null}

        <StatusView query={statusQuery} />

        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-sm text-muted-foreground">
            {list.total === 1 ? '1 volume' : `${list.total} volumes`}
          </p>
          <Button
            type="button"
            size="sm"
            className="w-full sm:w-auto"
            disabled={readOnly}
            onClick={() => {
              createForm.reset()
              setCreateOpen(true)
            }}
          >
            <Plus className="mr-2 h-4 w-4" />
            Create volume
          </Button>
        </div>

        {list.volumes.length === 0 ? (
          <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
            No volumes yet. Create one, for example <code>data</code>, and
            attach it at <code>/data</code> so uploaded files survive the next
            deployment.
          </p>
        ) : (
          <ul className="divide-y rounded-lg border">
            {list.volumes.map((volume) => (
              <VolumeRow
                key={volume.id}
                volume={volume}
                disabled={readOnly}
                onAttach={() => openAttach(volume)}
                onDetach={() => setDetachTarget(volume)}
                onDelete={() => setDeleteTarget(volume)}
              />
            ))}
          </ul>
        )}

        {list.total > list.page_size ? (
          <ResponsivePagination
            page={page}
            pageSize={list.page_size}
            total={list.total}
            totalPages={totalPages}
            onPageChange={setPage}
            ariaLabel="Volume list pagination"
          />
        ) : null}
      </div>

      <Dialog
        open={createOpen}
        onOpenChange={(open) => {
          if (!create.isPending) setCreateOpen(open)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create volume</DialogTitle>
            <DialogDescription>
              A Docker named volume on the node that runs this environment.
              Nothing is mounted until you attach it.
            </DialogDescription>
          </DialogHeader>
          <Form {...createForm}>
            <form
              noValidate
              className="space-y-4"
              onSubmit={createForm.handleSubmit((values) =>
                create.mutate(values)
              )}
            >
              <FormField
                control={createForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input placeholder="data" autoComplete="off" {...field} />
                    </FormControl>
                    <FormDescription>
                      2-64 lowercase letters, digits, &quot;_&quot;,
                      &quot;.&quot; or &quot;-&quot;; unique in this
                      environment.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={createForm.control}
                name="sizeMb"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Size in MB (optional)</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="1024"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={createForm.control}
                name="retainOnDelete"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-start gap-2 space-y-0">
                    <FormControl>
                      <Checkbox
                        checked={field.value}
                        onCheckedChange={(value) =>
                          field.onChange(value === true)
                        }
                      />
                    </FormControl>
                    <div className="space-y-1">
                      <FormLabel>
                        Keep the data when the volume is deleted
                      </FormLabel>
                      <FormDescription>
                        Deleting the volume then removes only its record; the
                        Docker volume stays on its node.
                      </FormDescription>
                    </div>
                  </FormItem>
                )}
              />
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={create.isPending}
                  onClick={() => setCreateOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={create.isPending}>
                  {create.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : null}
                  Create volume
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      <Dialog
        open={attachTarget !== null}
        onOpenChange={(open) => {
          if (!open && !attach.isPending) setAttachTarget(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Attach {attachTarget?.name ?? 'volume'}</DialogTitle>
            <DialogDescription>
              Every replica mounts it at this path from the next deployment. An
              attached volume pins the environment to the node that holds its
              data.
            </DialogDescription>
          </DialogHeader>
          <Form {...attachForm}>
            <form
              noValidate
              className="space-y-4"
              onSubmit={attachForm.handleSubmit((values) => {
                if (attachTarget)
                  attach.mutate({ volume: attachTarget, values })
              })}
            >
              <FormField
                control={attachForm.control}
                name="mountPath"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Mount path</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="/data"
                        autoComplete="off"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      An absolute path inside the container, e.g. /data or
                      /app/uploads.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={attachForm.control}
                name="readOnly"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center gap-2 space-y-0">
                    <FormControl>
                      <Checkbox
                        checked={field.value}
                        onCheckedChange={(value) =>
                          field.onChange(value === true)
                        }
                      />
                    </FormControl>
                    <FormLabel>Mount read-only</FormLabel>
                  </FormItem>
                )}
              />
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={attach.isPending}
                  onClick={() => setAttachTarget(null)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={attach.isPending}>
                  {attach.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : null}
                  Attach
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      <ConfirmDeleteDialog
        open={detachTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDetachTarget(null)
        }}
        resourceType="volume"
        resourceName={detachTarget?.name ?? ''}
        verb="Detach"
        irreversible={false}
        consequences={`Its data is kept. From the next deployment, replicas no longer mount it${
          detachTarget?.attachment
            ? ` at ${detachTarget.attachment.mount_path}`
            : ''
        }, and the environment is no longer pinned by it.`}
        isPending={detach.isPending}
        onConfirm={() => {
          if (detachTarget) detach.mutate(detachTarget)
        }}
      />

      <ConfirmDeleteDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        resourceType="volume"
        resourceName={deleteTarget?.name ?? ''}
        consequences={
          deleteTarget ? volumeDeleteConsequence(deleteTarget) : undefined
        }
        irreversible={!(deleteTarget?.retain_on_delete ?? false)}
        requireTypedName={
          deleteTarget !== null &&
          !deleteTarget.retain_on_delete &&
          deleteTarget.node_id != null
        }
        isPending={remove.isPending}
        onConfirm={() => {
          if (deleteTarget) remove.mutate(deleteTarget)
        }}
      />
    </Section>
  )
}

function Section({ children }: { children: ReactNode }) {
  return (
    <SettingsSection title={TITLE} description={DESCRIPTION} icon={HardDrive}>
      {children}
    </SettingsSection>
  )
}

function AdminOnlyNotice({ detail }: { detail?: string }) {
  return (
    <Callout tone="info" title="Instance administrators only">
      <p>
        An instance administrator can give this environment volumes, for example
        a <code>data</code> volume mounted at <code>/data</code> so uploaded
        files survive deployments. Ask an administrator to set one up.
      </p>
      {detail ? <p className="mt-1 text-muted-foreground">{detail}</p> : null}
    </Callout>
  )
}

/** Whether the next deployment mounts the attached volumes, and why not. */
function StatusView({
  query,
}: {
  query: UseQueryResult<EnvironmentVolumeStatus, Error>
}) {
  if (query.isPending) {
    return <Skeleton className="h-14 w-full" aria-busy="true" />
  }
  if (query.isError) {
    return (
      <ErrorAlert
        title="Could not check whether volumes are mounted"
        description={problemDetail(
          query.error,
          'The volume status could not be loaded.'
        )}
        retry={() => query.refetch()}
        retrying={query.isFetching}
      />
    )
  }
  const status = query.data
  if (status.active) {
    return (
      <Callout tone="success" title="Mounted on the next deployment">
        The next deployment mounts{' '}
        {status.attached === 1
          ? '1 attached volume'
          : `${status.attached} attached volumes`}
        .
      </Callout>
    )
  }
  const fixInWorkloadPlane = status.workload_mode !== 'enforce'
  const requirement =
    'Volumes are mounted only when the workload mode is Enforce, the environment runs one replica, and the data lives on the control plane.'
  if (status.attached === 0) {
    return (
      <Callout tone="info" title="No volume attached">
        <p>{requirement}</p>
        {status.reason ? (
          <p className="mt-1 text-muted-foreground">{status.reason}</p>
        ) : null}
      </Callout>
    )
  }
  return (
    <Callout tone="warning" title="Attached volumes would not be mounted">
      <p>{status.reason ?? requirement}</p>
      <p className="mt-1 text-muted-foreground">
        Until this is fixed, the next deployment of this environment fails with
        this reason.
      </p>
      {fixInWorkloadPlane ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="mt-2"
          onClick={revealWorkloadPlane}
        >
          Open workload plane settings
        </Button>
      ) : null}
    </Callout>
  )
}

function VolumeRow({
  volume,
  disabled,
  onAttach,
  onDetach,
  onDelete,
}: {
  volume: EnvironmentVolume
  disabled: boolean
  onAttach: () => void
  onDetach: () => void
  onDelete: () => void
}) {
  return (
    <li className="flex flex-col gap-3 p-3 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-sm font-medium">{volume.name}</span>
          {volume.attachment ? (
            <Badge variant="success-subtle">
              {volume.attachment.mount_path}
              {volume.attachment.read_only ? ' (read-only)' : ''}
            </Badge>
          ) : (
            <Badge variant="neutral-subtle">Detached</Badge>
          )}
          {volume.retain_on_delete ? (
            <Badge variant="outline">Data kept on delete</Badge>
          ) : null}
          {volume.size_mb != null ? (
            <Badge variant="outline">{volume.size_mb} MB</Badge>
          ) : null}
        </div>
        <p className="text-sm text-muted-foreground">
          Data {volumeLocation(volume)}
          {volume.pin_reason ? `; ${volume.pin_reason}` : ''}.
        </p>
      </div>
      <div className="flex shrink-0 flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={onAttach}
        >
          <Link2 className="h-4 w-4 sm:mr-2" />
          <span className="max-sm:sr-only">
            {volume.attachment ? 'Change mount' : 'Attach'}
          </span>
        </Button>
        {volume.attachment ? (
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={onDetach}
          >
            <Unlink className="h-4 w-4 sm:mr-2" />
            <span className="max-sm:sr-only">Detach</span>
          </Button>
        ) : null}
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={onDelete}
          className="text-destructive hover:text-destructive"
        >
          <Trash2 className="h-4 w-4 sm:mr-2" />
          <span className="max-sm:sr-only">Delete</span>
        </Button>
      </div>
    </li>
  )
}

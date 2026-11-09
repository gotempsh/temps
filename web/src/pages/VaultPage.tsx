// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { FolderLock, Plus, ArrowRight } from 'lucide-react'
import { toast } from 'sonner'
import {
  Button,
  Input,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  Skeleton,
} from '@temps-sdk/ui'
import {
  Callout,
  Field,
  PageContainer,
  PageHeader,
  PageState,
  ResponsivePagination,
} from '@temps-sdk/ds'
import {
  getProjectsOptions,
  getProjectOptions,
  listAccessVaultsOptions,
  accessCapabilityOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { createAccessVault } from '@/api/client'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { ServiceLogoStrip } from '@/components/service-access/service-providers'
import { problemDetail } from '@/lib/api-problem'

const vaultSchema = z.object({
  name: z.string().trim().min(1, 'Name is required').max(80),
})
export default function VaultPage() {
  const { projectId } = useParams()
  return <VaultCollection key={projectId ?? 'projects'} />
}
function VaultCollection() {
  const { projectId: rawId } = useParams()
  const projectId = Number(rawId)
  const valid =
    Number.isInteger(projectId) && projectId > 0 && projectId <= 2147483647
  const [params, setParams] = useSearchParams()
  const page = Math.min(
    1000000,
    Math.max(1, Math.floor(Number(params.get('page')) || 1))
  )
  const navigate = useNavigate()
  const client = useQueryClient()
  const [open, setOpen] = useState(false)
  const form = useForm<{ name: string }>({
    resolver: zodResolver(vaultSchema),
    defaultValues: { name: '' },
  })
  const projects = useQuery({
    ...getProjectsOptions({ query: { page, per_page: 20 } }),
    enabled: !rawId,
  })
  const project = useQuery({
    ...getProjectOptions({ path: { id: projectId } }),
    enabled: valid,
  })
  const vaults = useQuery({
    ...listAccessVaultsOptions({
      path: { project_id: projectId },
      query: { page, page_size: 20 },
    }),
    enabled: valid,
  })
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    enabled: valid,
  })
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs(
      valid
        ? [
            { label: 'Vault', href: '/vault' },
            { label: project.data?.name ?? 'Vaults' },
          ]
        : [{ label: 'Vault' }]
    )
    return () => setBreadcrumbs([])
  }, [valid, project.data?.name, setBreadcrumbs])
  useEffect(() => {
    if (
      !rawId &&
      projects.data &&
      page > 1 &&
      projects.data.projects.length === 0
    ) {
      const next = new URLSearchParams(params)
      next.delete('page')
      setParams(next, { replace: true })
      return
    }
    if (
      !rawId &&
      !params.has('choose') &&
      projects.data?.total === 1 &&
      projects.data.projects[0]
    )
      navigate(`/vault/projects/${projects.data.projects[0].id}`, {
        replace: true,
      })
  }, [rawId, projects.data, navigate, params, page, setParams])
  const create = useMutation({
    mutationFn: async (values: { name: string }) => {
      if (!valid || capability.isError || !capability.data?.can_manage_services)
        throw new Error('Project write permission is required')
      return createAccessVault({
        path: { project_id: projectId },
        body: { name: values.name, service_ids: [] },
        throwOnError: true,
      })
    },
    onSuccess: ({ data }) => {
      void client.invalidateQueries({ queryKey: [{ _id: 'listAccessVaults' }] })
      setOpen(false)
      form.reset()
      toast.success('Vault created')
      navigate(
        `/vault/projects/${projectId}/vaults/${encodeURIComponent(data.id)}`
      )
    },
  })
  const changePage = (page: number) => setParams({ page: String(page) })
  const error = rawId ? vaults.error : projects.error
  const total = rawId ? vaults.data?.total : projects.data?.total
  return (
    <PageContainer width="full">
      <PageHeader
        title="Vaults"
        actions={
          valid ? (
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" asChild>
                <Link to="/vault?choose=project">
                  {project.data?.name ?? 'Change project'}
                </Link>
              </Button>
              <Button
                disabled={
                  capability.isError || !capability.data?.can_manage_services
                }
                title={
                  !capability.data?.can_manage_services
                    ? 'Project write permission is required'
                    : undefined
                }
                onClick={() => setOpen(true)}
              >
                <Plus className="size-4" />
                Create vault
              </Button>
            </div>
          ) : undefined
        }
      />
      {error && (
        <Callout tone="error" title="Could not load vaults">
          <Button
            variant="outline"
            onClick={() => void (rawId ? vaults.refetch() : projects.refetch())}
          >
            Retry
          </Button>
        </Callout>
      )}
      {rawId && !valid ? (
        <Callout tone="error" title="Invalid project">
          Choose a project from{' '}
          <Link to="/vault" className="underline">
            Vault
          </Link>
          .
        </Callout>
      ) : !rawId ? (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Choose the project whose vaults you want to manage.
          </p>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
            {projects.data?.projects.map((p) => (
              <Link
                key={p.id}
                to={`/vault/projects/${p.id}`}
                className="flex items-center gap-3 rounded-lg border p-4 hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ServiceIcon service={p} size="sm" />
                <span className="min-w-0 flex-1 break-words font-medium">
                  {p.name}
                </span>
                <ArrowRight className="size-4" />
              </Link>
            ))}
          </div>
          {!projects.isPending && !projects.isError && total === 0 && (
            <PageState
              variant="empty"
              icon={FolderLock}
              title="No projects available"
              description="Create a project to organize its services in vaults."
              action={
                <Button asChild>
                  <Link to="/projects">Open projects</Link>
                </Button>
              }
            />
          )}
        </div>
      ) : (
        <div className="space-y-5">
          {vaults.isPending ? (
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-44 rounded-lg" />
              ))}
            </div>
          ) : vaults.data?.total === 0 ? (
            <PageState
              variant="empty"
              icon={FolderLock}
              title="Create your first vault"
              description="Keep services such as GitHub and Stripe together, then choose their permissions in access bundles."
              action={
                <Button
                  disabled={!capability.data?.can_manage_services}
                  onClick={() => setOpen(true)}
                >
                  <Plus className="size-4" />
                  Create vault
                </Button>
              }
            />
          ) : (
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
              {vaults.data?.items.map(({ vault, services }) => (
                <Link
                  key={vault.id}
                  to={`/vault/projects/${projectId}/vaults/${encodeURIComponent(vault.id)}`}
                  className="flex min-w-0 flex-col overflow-hidden rounded-lg border transition-colors hover:border-foreground/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <div className="flex items-center justify-between gap-3 p-4 pb-3">
                    <h2 className="truncate font-semibold">{vault.name}</h2>
                    <FolderLock className="size-4 shrink-0 text-muted-foreground" />
                  </div>
                  <div className="min-h-20 flex-1 px-4 pb-4">
                    {vault.service_ids.length ? (
                      <ServiceLogoStrip
                        services={services}
                        total={vault.service_ids.length}
                      />
                    ) : (
                      <div className="space-y-2">
                        <span className="flex size-9 items-center justify-center rounded-md border border-dashed">
                          <Plus className="size-4 text-muted-foreground" />
                        </span>
                        <p className="text-xs text-muted-foreground">
                          No services yet. Add your first service.
                        </p>
                      </div>
                    )}
                  </div>
                  <div className="flex items-center justify-between border-t px-4 py-3 text-xs text-muted-foreground">
                    <span>
                      {vault.service_ids.length}{' '}
                      {vault.service_ids.length === 1 ? 'service' : 'services'}
                    </span>
                    <span className="flex items-center gap-1">
                      Open vault
                      <ArrowRight className="size-3" />
                    </span>
                  </div>
                </Link>
              ))}
            </div>
          )}
        </div>
      )}
      {(total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={total ?? 0}
          totalPages={Math.ceil((total ?? 0) / 20)}
          onPageChange={changePage}
        />
      )}
      <Dialog
        open={open}
        onOpenChange={(next) => {
          if (!create.isPending) setOpen(next)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create vault</DialogTitle>
          </DialogHeader>
          <form
            className="space-y-4"
            onSubmit={form.handleSubmit((v) => create.mutate(v))}
          >
            <Field label="Name" error={form.formState.errors.name?.message}>
              {(props) => (
                <Input
                  {...form.register('name')}
                  {...props}
                  placeholder="Development"
                  disabled={create.isPending}
                  autoFocus
                />
              )}
            </Field>
            {create.isError && (
              <Callout tone="error" title="Could not create vault">
                {problemDetail(create.error, 'Try again.')}
              </Callout>
            )}
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => setOpen(false)}
                disabled={create.isPending}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={
                  create.isPending || !capability.data?.can_manage_services
                }
              >
                Create vault
              </Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}

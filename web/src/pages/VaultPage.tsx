// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
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
  listAccessVaultsGeneralOptions,
  generalAccessCapabilityOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { createAccessVaultGeneral } from '@/api/client'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { ServiceLogoStrip } from '@/components/service-access/service-providers'
import { problemDetail } from '@/lib/api-problem'

const vaultSchema = z.object({
  name: z.string().trim().min(1, 'Name is required').max(80),
})
export default function VaultPage() {
  return <VaultCollection />
}
function VaultCollection() {
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
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
  const capability = useQuery({
    ...generalAccessCapabilityOptions(),
  })
  const vaults = useQuery({
    ...listAccessVaultsGeneralOptions({
      query: { page, page_size: 20 },
    }),
    enabled: capability.data?.can_manage_services === true,
  })
  useEffect(() => {
    if (vaults.data && page > Math.max(1, Math.ceil(vaults.data.total / 20)))
      setParams({})
  }, [vaults.data, page, setParams])
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([{ label: 'Vaults' }])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs])
  const create = useMutation({
    mutationFn: async (values: { name: string }) => {
      if (capability.isError || !capability.data?.can_manage_services)
        throw new Error('Instance administrator permission is required')
      return createAccessVaultGeneral({
        body: { name: values.name, service_ids: [] },
        throwOnError: true,
      })
    },
    onSuccess: ({ data }) => {
      void client.invalidateQueries({
        queryKey: [{ _id: 'listAccessVaultsGeneral' }],
      })
      if (!mounted.current) return
      setOpen(false)
      form.reset()
      toast.success('Vault created')
      navigate(`/vault/vaults/${encodeURIComponent(data.id)}`)
    },
  })
  const changePage = (page: number) => setParams({ page: String(page) })
  const error = vaults.error
  const total = vaults.data?.total
  return (
    <PageContainer width="full">
      <PageHeader
        title="Vaults"
        actions={
          <Button
            disabled={
              capability.isError || !capability.data?.can_manage_services
            }
            onClick={() => setOpen(true)}
          >
            <Plus className="size-4" />
            Create vault
          </Button>
        }
      />
      {capability.data && !capability.data.can_manage_services && (
        <Callout title="Instance administrator permission is required">
          Administrators manage vaults and links. Open a sandbox to request
          access from its available vaults, or request access in AI chat.
          <Button asChild variant="outline">
            <Link to="/sandboxes">Open sandboxes</Link>
          </Button>
        </Callout>
      )}
      {error && (
        <Callout tone="error" title="Could not load vaults">
          <Button variant="outline" onClick={() => void vaults.refetch()}>
            Retry
          </Button>
        </Callout>
      )}
      {capability.data?.can_manage_services && (
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
                  to={`/vault/vaults/${encodeURIComponent(vault.id)}`}
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

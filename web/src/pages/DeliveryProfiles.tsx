// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createDeliveryProfile,
  deleteDeliveryProfile,
  listDeliveryProfiles,
  getDeliveryCapabilities,
} from '@/api/client'
import {
  deliveryError,
  requireDeliveryData,
} from '@/components/domains/delivery-errors'
import {
  DELIVERY_PROFILES_QUERY_ROOT,
  deliveryCapabilitiesQueryKey,
  deliveryPageCount,
  deliveryProfilePickerQueryKey,
  fetchDeliveryProfilePicker,
  mayHaveDeliveryProfileOfKind,
} from '@/components/domains/delivery-queries'
import { DeliveryProviderChoice } from '@/components/domains/DeliveryProviderChoice'
import type { DeliveryProviderChoiceValue } from '@/components/domains/DeliveryProviderChoice'
import { CloudflareIcon } from '@/components/icons/DnsProviderIcons'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  getPlatformSettings,
  updatePlatformSettings,
} from '@/api/platformSettings'
import { Button } from '@/components/ui/button'
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
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { EmptyState } from '@/components/ui/empty-state'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { zodResolver } from '@hookform/resolvers/zod'
import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Globe, Plus, Trash2 } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { RecordLink } from '@temps-sdk/ds'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  name: z.string().trim().min(1, 'Name this profile').max(100),
  provider_kind: z.enum(['direct', 'cloudflare', 'bunny']),
  bunny_pull_zone_id: z.string().optional(),
  bunny_api_key: z.string().optional(),
})
type ProfileForm = z.infer<typeof schema>

const PROFILES_PAGE_SIZE = 20

export default function DeliveryProfiles() {
  usePageTitle('Delivery profiles')
  const { setBreadcrumbs } = useBreadcrumbs()
  const client = useQueryClient()
  const [open, setOpen] = useState(false)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [page, setPage] = useState(1)
  const listQuery = {
    page,
    page_size: PROFILES_PAGE_SIZE,
    sort_by: 'created_at',
    sort_order: 'desc',
  }
  const profiles = useQuery({
    queryKey: [DELIVERY_PROFILES_QUERY_ROOT, 'list', listQuery],
    queryFn: async () =>
      requireDeliveryData(await listDeliveryProfiles({ query: listQuery })),
    // Keep the current rows on screen while the next page loads.
    placeholderData: keepPreviousData,
  })
  // Whether a profile of each kind exists anywhere, not only on the page
  // being viewed: the new-project default below depends on it.
  const profileCatalog = useQuery({
    queryKey: deliveryProfilePickerQueryKey,
    queryFn: fetchDeliveryProfilePicker,
  })
  const total = profiles.data?.total ?? 0
  const totalPages = deliveryPageCount(total, PROFILES_PAGE_SIZE)
  // Deleting the last profile on the last page leaves it past the end: move
  // to the last page that still has rows rather than showing an empty one.
  if (profiles.data && !profiles.isPlaceholderData && page > totalPages)
    setPage(totalPages)
  const rows = profiles.data?.items ?? []
  const loadingRows = profiles.isPending || (total > 0 && rows.length === 0)
  const platformSettings = useQuery({
    queryKey: ['platform-settings'],
    queryFn: getPlatformSettings,
  })
  const saveDefault = useMutation({
    mutationFn: (provider: DeliveryProviderChoiceValue) =>
      updatePlatformSettings({
        cloudflare_new_projects: provider === 'cloudflare',
        bunny_new_projects: provider === 'bunny',
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ['platform-settings'] })
      toast.success('Delivery default updated for future projects')
    },
    onError: (error: Error) => toast.error(error.message),
  })
  const form = useForm<ProfileForm>({
    resolver: zodResolver(schema),
    defaultValues: {
      name: '',
      provider_kind: 'direct',
      bunny_pull_zone_id: '',
      bunny_api_key: '',
    },
  })
  const providerKind = useWatch({
    control: form.control,
    name: 'provider_kind',
  })
  const capabilities = useQuery({
    queryKey: deliveryCapabilitiesQueryKey,
    queryFn: async () => requireDeliveryData(await getDeliveryCapabilities()),
  })
  const selectedCapability = capabilities.data?.find(
    (capability) => capability.provider_kind === providerKind
  )
  const cloudflareReady = Boolean(
    mayHaveDeliveryProfileOfKind(profileCatalog.data, 'cloudflare') &&
    capabilities.data?.some(
      (capability) =>
        capability.provider_kind === 'cloudflare' && capability.configured
    )
  )
  const bunnyReady = Boolean(
    mayHaveDeliveryProfileOfKind(profileCatalog.data, 'bunny') &&
    capabilities.data?.some(
      (capability) =>
        capability.provider_kind === 'bunny' && capability.configured
    )
  )
  const create = useMutation({
    mutationFn: async (body: ProfileForm) => {
      if (
        body.provider_kind === 'bunny' &&
        (!body.bunny_pull_zone_id || !body.bunny_api_key)
      )
        throw new Error('Enter the Bunny Pull Zone ID and API key')
      if (
        body.provider_kind === 'bunny' &&
        (!Number.isSafeInteger(Number(body.bunny_pull_zone_id)) ||
          Number(body.bunny_pull_zone_id) <= 0)
      )
        throw new Error('Enter a positive Bunny Pull Zone ID')
      return requireDeliveryData(
        await createDeliveryProfile({
          body: {
            name: body.name,
            provider_kind: body.provider_kind,
            bunny_pull_zone_id:
              body.provider_kind === 'bunny'
                ? Number(body.bunny_pull_zone_id)
                : null,
            bunny_api_key:
              body.provider_kind === 'bunny' ? body.bunny_api_key : null,
          },
        })
      )
    },
    onSuccess: () => {
      client.invalidateQueries({ queryKey: [DELIVERY_PROFILES_QUERY_ROOT] })
      // Newest first: the new profile is at the top of the first page.
      setPage(1)
      setOpen(false)
      form.reset()
      toast.success('Delivery profile created')
    },
  })
  const remove = useMutation({
    mutationFn: async (profile_id: number) => {
      const response = await deleteDeliveryProfile({ path: { profile_id } })
      if (response.error) throw new Error(deliveryError(response.error))
    },
    onSuccess: () => {
      client.invalidateQueries({ queryKey: [DELIVERY_PROFILES_QUERY_ROOT] })
      setDeleteId(null)
      toast.success('Delivery profile deleted')
    },
  })
  useEffect(
    () => setBreadcrumbs([{ label: 'Delivery profiles' }]),
    [setBreadcrumbs]
  )
  return (
    <PageContainer>
      <PageHeader
        title="Delivery profiles"
        description="Choose how project traffic reaches your applications. Bunny profiles also store Pull Zone configuration."
        actions={
          <Button
            onClick={() => {
              create.reset()
              setOpen(true)
            }}
          >
            <Plus className="mr-2 size-4" />
            Create profile
          </Button>
        }
      />
      <div className="flex flex-col justify-between gap-3 rounded-lg border bg-card p-4 sm:flex-row sm:items-center">
        <div>
          <p className="text-sm font-medium">
            DNS and delivery are configured separately
          </p>
          <p className="mt-1 text-sm text-muted-foreground">
            Connect a DNS provider, then select a delivery profile in your
            project’s Domains page.{' '}
            <Link className="underline" to="/dns-providers/add?provider=bunny">
              Connect Bunny DNS
            </Link>{' '}
            if Bunny hosts your zone.
          </p>
        </div>
        <Button variant="outline" asChild>
          <Link to="/dns-providers">Manage DNS providers</Link>
        </Button>
      </div>
      <section className="space-y-4 border-b pb-6">
        <div>
          <p className="font-medium">Default delivery for new projects</p>
          <p className="text-sm text-muted-foreground">
            Applies when a project is created. Existing projects are never
            changed. Each project can override this choice during creation or in
            Domains.
          </p>
          {platformSettings.isError && (
            <p className="text-sm text-muted-foreground">
              Instance settings access is required to change this default.
            </p>
          )}
          {!cloudflareReady && (
            <p className="text-sm text-muted-foreground">
              Create a Cloudflare delivery profile and connect an active DNS
              provider first.
            </p>
          )}
          {!bunnyReady && (
            <p className="text-sm text-muted-foreground">
              <Button
                type="button"
                variant="link"
                className="h-auto p-0"
                onClick={() => {
                  create.reset()
                  form.reset({
                    name: '',
                    provider_kind: 'bunny',
                    bunny_pull_zone_id: '',
                    bunny_api_key: '',
                  })
                  setOpen(true)
                }}
              >
                Set up bunny.net CDN
              </Button>{' '}
              with an active Pull Zone and account API key. Bunny DNS is
              optional; use any supported DNS provider for your zone.
            </p>
          )}
        </div>
        <DeliveryProviderChoice
          value={
            platformSettings.data?.cloudflare_new_projects
              ? 'cloudflare'
              : platformSettings.data?.bunny_new_projects
                ? 'bunny'
                : 'none'
          }
          onChange={(selected) => saveDefault.mutate(selected)}
          cloudflareConfigured={cloudflareReady}
          bunnyConfigured={bunnyReady}
          disabled={
            platformSettings.isPending ||
            platformSettings.isError ||
            saveDefault.isPending
          }
        />
      </section>
      <div
        className="rounded-lg border bg-card text-card-foreground"
        aria-busy={loadingRows || profiles.isPlaceholderData}
      >
        {profiles.isError && (
          <div className={rows.length > 0 ? 'border-b p-4' : 'p-4'}>
            <Alert variant="destructive">
              <AlertDescription>
                {deliveryError(profiles.error)}{' '}
                <Button variant="link" onClick={() => profiles.refetch()}>
                  Retry
                </Button>
              </AlertDescription>
            </Alert>
          </div>
        )}
        {loadingRows ? (
          <ProfilesTable>
            <ProfileSkeletonRows />
          </ProfilesTable>
        ) : rows.length > 0 ? (
          <ProfilesTable
            bodyClassName={
              profiles.isPlaceholderData
                ? 'opacity-60 transition-opacity'
                : undefined
            }
          >
            {rows.map((profile) => (
              <TableRow key={profile.id}>
                <TableCell>
                  <div className="flex min-w-0 items-center gap-3">
                    {profile.provider_kind === 'cloudflare' ? (
                      <CloudflareIcon className="size-5 shrink-0 text-[#f48120]" />
                    ) : profile.provider_kind === 'bunny' ? (
                      <img
                        src="/providers/bunny-official.svg"
                        alt=""
                        className="size-5 shrink-0"
                      />
                    ) : (
                      <Globe className="size-5 shrink-0" />
                    )}
                    <div className="min-w-0">
                      <RecordLink
                        to={`/delivery-profiles/${profile.id}`}
                        aria-label={`View ${profile.name} details`}
                      >
                        {profile.name}
                      </RecordLink>
                      <p className="text-xs text-muted-foreground sm:hidden">
                        {profile.provider_kind === 'bunny'
                          ? 'bunny.net CDN'
                          : profile.provider_kind === 'cloudflare'
                            ? 'Cloudflare proxy'
                            : 'Direct'}
                      </p>
                    </div>
                  </div>
                </TableCell>
                <TableCell className="hidden sm:table-cell">
                  {profile.provider_kind === 'bunny'
                    ? 'bunny.net CDN'
                    : profile.provider_kind === 'cloudflare'
                      ? 'Cloudflare proxy'
                      : 'Direct to origin'}
                </TableCell>
                <TableCell className="hidden text-muted-foreground md:table-cell">
                  {profile.provider_kind !== 'bunny'
                    ? 'DNS connection selected per domain'
                    : profile.bunny_pull_zone_id == null
                      ? 'Pull Zone visible with DNS provider read access'
                      : `Pull Zone ${profile.bunny_pull_zone_id}`}
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Delete ${profile.name}`}
                    onClick={() => {
                      remove.reset()
                      setDeleteId(profile.id)
                    }}
                  >
                    <Trash2 className="size-4" />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </ProfilesTable>
        ) : profiles.data ? (
          <EmptyState
            size="compact"
            icon={Globe}
            title="No delivery profiles yet"
            description="Create a profile to choose Direct, Cloudflare, or Bunny delivery for new domain setups."
            action={
              <Button onClick={() => setOpen(true)}>Create profile</Button>
            }
          />
        ) : null}
      </div>
      {profiles.data && totalPages > 1 && (
        <ResponsivePagination
          page={page}
          pageSize={PROFILES_PAGE_SIZE}
          total={total}
          totalPages={totalPages}
          onPageChange={setPage}
          ariaLabel="Delivery profile pagination"
        />
      )}
      <Dialog
        open={open}
        onOpenChange={(value) => {
          if (!create.isPending) setOpen(value)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create delivery profile</DialogTitle>
            <DialogDescription>
              Projects can share this profile. Existing DNS records and
              certificates stay as configured until you apply a domain setup.
            </DialogDescription>
          </DialogHeader>
          <Form {...form}>
            <form
              className="space-y-5"
              onSubmit={form.handleSubmit((body) => create.mutate(body))}
            >
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Profile name</FormLabel>
                    <FormControl>
                      <Input placeholder="Storefront delivery" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="provider_kind"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Delivery</FormLabel>
                    <FormControl>
                      <div className="grid grid-cols-3 gap-2">
                        {capabilities.data?.map((capability) => (
                          <button
                            key={capability.provider_kind}
                            type="button"
                            disabled={!capability.supported}
                            aria-pressed={
                              field.value === capability.provider_kind
                            }
                            onClick={() =>
                              field.onChange(capability.provider_kind)
                            }
                            className={`flex min-h-24 min-w-0 flex-col justify-between rounded-lg border p-3 text-left text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${field.value === capability.provider_kind ? 'border-primary bg-primary/5 ring-1 ring-primary' : 'hover:bg-muted'}`}
                          >
                            {capability.provider_kind === 'bunny' && (
                              <img
                                src="/providers/bunny-official.svg"
                                alt=""
                                className="size-7"
                              />
                            )}
                            {capability.provider_kind === 'cloudflare' && (
                              <>
                                <img
                                  src="/providers/cloudflare-official.png"
                                  alt=""
                                  className="h-7 w-20 object-contain object-left dark:hidden"
                                />
                                <CloudflareIcon className="hidden size-7 dark:block" />
                              </>
                            )}
                            {capability.provider_kind === 'direct' && (
                              <Globe className="size-6" aria-hidden="true" />
                            )}
                            <span className="font-medium leading-tight">
                              {capability.name}
                            </span>
                          </button>
                        ))}
                      </div>
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {selectedCapability && (
                <div className="rounded-md bg-muted/40 p-3 text-xs text-muted-foreground">
                  <p className="font-medium text-foreground">
                    {selectedCapability.name} requirements
                  </p>
                  <ul className="mt-1 list-disc space-y-1 pl-4">
                    {selectedCapability.requirements.map((requirement) => (
                      <li key={requirement}>{requirement}</li>
                    ))}
                  </ul>
                </div>
              )}
              {providerKind === 'bunny' && (
                <div className="space-y-4">
                  <p className="text-sm text-muted-foreground">
                    Deliver a hostname such as app.example.com through Bunny
                    CDN. Use an active Pull Zone whose origin points to the
                    Temps edge target and has Add Host Header enabled. Its API
                    key is encrypted at rest and never shown again. DNS is
                    configured separately; a Bunny DNS connection is optional.
                  </p>
                  <FormField
                    control={form.control}
                    name="bunny_pull_zone_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Pull Zone ID</FormLabel>
                        <FormControl>
                          <Input
                            inputMode="numeric"
                            placeholder="12345"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="bunny_api_key"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Bunny API key</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            autoComplete="off"
                            placeholder="Enter your account API key"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
              )}
              {capabilities.isPending && <Skeleton className="h-24 w-full" />}
              {capabilities.isError && (
                <Alert variant="destructive">
                  <AlertDescription>
                    {deliveryError(capabilities.error)}{' '}
                    <Button
                      type="button"
                      variant="link"
                      onClick={() => capabilities.refetch()}
                    >
                      Retry
                    </Button>
                  </AlertDescription>
                </Alert>
              )}
              {capabilities.data
                ?.filter(
                  (capability) =>
                    capability.provider_kind !== 'bunny' &&
                    !capability.configured &&
                    capability.setup_path
                )
                .map((capability) => (
                  <p
                    key={capability.provider_kind}
                    className="text-sm text-muted-foreground"
                  >
                    <Link
                      className="underline"
                      to={capability.setup_path ?? '/dns-providers'}
                    >
                      Set up {capability.name}
                    </Link>{' '}
                    before applying this profile to a domain.
                  </p>
                ))}
              {create.isError && (
                <Alert variant="destructive">
                  <AlertDescription>
                    {deliveryError(create.error)}
                  </AlertDescription>
                </Alert>
              )}
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={create.isPending}
                  onClick={() => setOpen(false)}
                >
                  Cancel
                </Button>
                <Button
                  disabled={create.isPending || !capabilities.isSuccess}
                  type="submit"
                >
                  {create.isPending ? 'Creating…' : 'Create profile'}
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>
      <Dialog
        open={deleteId !== null}
        onOpenChange={(value) => {
          if (!value && !remove.isPending) setDeleteId(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete delivery profile?</DialogTitle>
            <DialogDescription>
              Profiles used by project defaults, environment overrides, or
              domain bindings cannot be deleted.
            </DialogDescription>
          </DialogHeader>
          {remove.isError && (
            <Alert variant="destructive">
              <AlertDescription>{deliveryError(remove.error)}</AlertDescription>
            </Alert>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              disabled={remove.isPending}
              onClick={() => setDeleteId(null)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={remove.isPending}
              onClick={() => {
                if (deleteId !== null) remove.mutate(deleteId)
              }}
            >
              {remove.isPending ? 'Deleting…' : 'Delete profile'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}

function ProfilesTable({
  children,
  bodyClassName,
}: {
  children: ReactNode
  bodyClassName?: string
}) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Profile</TableHead>
          <TableHead className="hidden sm:table-cell">Delivery</TableHead>
          <TableHead className="hidden md:table-cell">Configuration</TableHead>
          <TableHead className="w-16 text-right">Actions</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody className={bodyClassName}>{children}</TableBody>
    </Table>
  )
}

/** Placeholder rows with the same columns as a loaded profile row. */
function ProfileSkeletonRows() {
  return Array.from({ length: 3 }, (_, index) => (
    <TableRow key={index}>
      <TableCell>
        <div className="flex items-center gap-3">
          <Skeleton className="size-5 shrink-0 rounded-full" />
          <Skeleton className="h-4 w-40" />
        </div>
      </TableCell>
      <TableCell className="hidden sm:table-cell">
        <Skeleton className="h-4 w-28" />
      </TableCell>
      <TableCell className="hidden md:table-cell">
        <Skeleton className="h-4 w-48" />
      </TableCell>
      <TableCell className="text-right">
        <Skeleton className="ml-auto size-8" />
      </TableCell>
    </TableRow>
  ))
}

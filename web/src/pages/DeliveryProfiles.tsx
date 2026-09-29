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
import { DeliveryProviderChoice } from '@/components/domains/DeliveryProviderChoice'
import type { DeliveryProviderChoiceValue } from '@/components/domains/DeliveryProviderChoice'
import { CloudflareIcon } from '@/components/icons/DnsProviderIcons'
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
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Globe, Plus, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  name: z.string().trim().min(1, 'Name this profile').max(100),
  provider_kind: z.enum(['direct', 'cloudflare', 'bunny']),
  bunny_pull_zone_id: z.string().optional(),
  bunny_api_key: z.string().optional(),
})
type ProfileForm = z.infer<typeof schema>

export default function DeliveryProfiles() {
  usePageTitle('Delivery profiles')
  const { setBreadcrumbs } = useBreadcrumbs()
  const client = useQueryClient()
  const [open, setOpen] = useState(false)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [detailsId, setDetailsId] = useState<number | null>(null)
  const profiles = useQuery({
    queryKey: ['delivery-profiles'],
    queryFn: async () => requireDeliveryData(await listDeliveryProfiles()),
  })
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
    queryKey: ['delivery-capabilities'],
    queryFn: async () => requireDeliveryData(await getDeliveryCapabilities()),
  })
  const selectedCapability = capabilities.data?.find(
    (capability) => capability.provider_kind === providerKind
  )
  const cloudflareReady = Boolean(
    profiles.data?.some((profile) => profile.provider_kind === 'cloudflare') &&
    capabilities.data?.some(
      (capability) =>
        capability.provider_kind === 'cloudflare' && capability.configured
    )
  )
  const bunnyReady = Boolean(
    profiles.data?.some((profile) => profile.provider_kind === 'bunny') &&
    capabilities.data?.some(
      (capability) =>
        capability.provider_kind === 'bunny' && capability.configured
    )
  )
  const detailsProfile = profiles.data?.find(
    (profile) => profile.id === detailsId
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
      client.invalidateQueries({ queryKey: ['delivery-profiles'] })
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
      client.invalidateQueries({ queryKey: ['delivery-profiles'] })
      setDeleteId(null)
      toast.success('Delivery profile deleted')
    },
  })
  useEffect(
    () => setBreadcrumbs([{ label: 'Delivery profiles' }]),
    [setBreadcrumbs]
  )
  return (
    <div className="mx-auto w-full max-w-5xl space-y-8 p-4 sm:p-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-start">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            Delivery profiles
          </h1>
          <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
            A profile is a reusable delivery choice for project defaults and
            domains. Cloudflare uses the DNS connection selected for each
            domain; Bunny also stores a Pull Zone and API key.
          </p>
        </div>
        <Button
          onClick={() => {
            create.reset()
            setOpen(true)
          }}
        >
          <Plus className="mr-2 size-4" />
          Create profile
        </Button>
      </header>
      <div className="flex flex-col justify-between gap-3 rounded-lg border bg-muted/30 p-4 sm:flex-row sm:items-center">
        <div>
          <p className="text-sm font-medium">
            DNS and delivery are configured separately
          </p>
          <p className="mt-1 text-sm text-muted-foreground">
            Connect a DNS provider, then select a delivery profile in your
            project’s Domains page.
          </p>
        </div>
        <Button variant="outline" asChild>
          <Link to="/dns-providers">Manage DNS providers</Link>
        </Button>
      </div>
      <div className="space-y-4 rounded-lg border p-4">
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
              Create a Bunny profile using an active Pull Zone and API key
              first.
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
      </div>
      {profiles.isPending ? (
        <div className="space-y-3">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
        </div>
      ) : profiles.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(profiles.error)}{' '}
            <Button variant="link" onClick={() => profiles.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : profiles.data?.length ? (
        <ul className="divide-y rounded-lg border">
          {profiles.data.map((profile) => (
            <li
              key={profile.id}
              className="flex items-center justify-between gap-4 p-4"
            >
              <div className="flex min-w-0 items-start gap-3">
                {profile.provider_kind === 'cloudflare' ? (
                  <CloudflareIcon className="mt-1 size-5 shrink-0 text-[#f48120]" />
                ) : profile.provider_kind === 'bunny' ? (
                  <img
                    src="/providers/bunny-official.svg"
                    alt=""
                    className="mt-1 size-5 shrink-0"
                  />
                ) : (
                  <Globe className="mt-1 size-5 shrink-0" />
                )}
                <div className="min-w-0">
                  <p className="break-words font-medium">{profile.name}</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {profile.provider_kind === 'cloudflare'
                      ? 'Cloudflare proxy · DNS connection selected per domain'
                      : profile.provider_kind === 'bunny'
                        ? `bunny.net CDN · Pull Zone ${profile.bunny_pull_zone_id}`
                        : 'Direct to origin · DNS connection selected per domain'}
                  </p>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setDetailsId(profile.id)}
                >
                  View details
                </Button>
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
              </div>
            </li>
          ))}
        </ul>
      ) : (
        <div className="rounded-lg border border-dashed px-6 py-12 text-center">
          <Globe className="mx-auto mb-4 size-7 text-muted-foreground" />
          <h2 className="font-medium">Set up your first delivery profile</h2>
          <p className="mx-auto mt-2 max-w-md text-sm text-muted-foreground">
            For example, use Direct for your API, Cloudflare for your
            storefront, or Bunny for another app. Creating a profile does not
            change any existing domain.
          </p>
          <Button
            className="mt-5"
            variant="outline"
            onClick={() => setOpen(true)}
          >
            Create profile
          </Button>
        </div>
      )}
      <Dialog
        open={detailsId !== null}
        onOpenChange={(value) => !value && setDetailsId(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {detailsProfile?.name ?? 'Delivery profile'}
            </DialogTitle>
            <DialogDescription>
              This reusable choice can be assigned to projects, environments,
              and domains. A profile does not change existing domains by itself.
            </DialogDescription>
          </DialogHeader>
          {detailsProfile && (
            <div className="space-y-4 text-sm">
              <div className="grid grid-cols-[8rem_1fr] gap-x-4 gap-y-2 rounded-lg border p-4">
                <span className="text-muted-foreground">Delivery</span>
                <span className="font-medium">
                  {detailsProfile.provider_kind === 'cloudflare'
                    ? 'Cloudflare proxy'
                    : detailsProfile.provider_kind === 'bunny'
                      ? 'bunny.net CDN'
                      : 'Direct to Temps origin'}
                </span>
                {detailsProfile.provider_kind === 'bunny' ? (
                  <>
                    <span className="text-muted-foreground">Pull Zone ID</span>
                    <span>{detailsProfile.bunny_pull_zone_id}</span>
                    <span className="text-muted-foreground">
                      Pull Zone hostname
                    </span>
                    <span className="break-all">
                      {detailsProfile.bunny_hostname}
                    </span>
                    <span className="text-muted-foreground">API key</span>
                    <span>Stored securely; the key cannot be viewed again</span>
                  </>
                ) : (
                  <>
                    <span className="text-muted-foreground">
                      DNS connection
                    </span>
                    <span>Selected for each domain during domain setup</span>
                  </>
                )}
              </div>
              <p className="text-muted-foreground">
                {detailsProfile.provider_kind === 'cloudflare'
                  ? 'Cloudflare credentials and zones are managed in DNS providers. This profile only selects Cloudflare proxy delivery; other Cloudflare profiles behave the same way.'
                  : detailsProfile.provider_kind === 'bunny'
                    ? 'The Pull Zone and API key were verified when this profile was created. Create a new profile to use another Pull Zone or key.'
                    : 'Direct delivery has no profile-specific credentials. Choose the DNS connection and origin when setting up a domain.'}
              </p>
            </div>
          )}
          <DialogFooter>
            {detailsProfile?.provider_kind === 'cloudflare' && (
              <Button variant="outline" asChild>
                <Link to="/dns-providers" onClick={() => setDetailsId(null)}>
                  Manage DNS providers
                </Link>
              </Button>
            )}
            <Button onClick={() => setDetailsId(null)}>Done</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
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
                <div className="space-y-4 rounded-md border p-4">
                  <p className="text-sm text-muted-foreground">
                    Use a Pull Zone whose origin points to the Temps edge target
                    and has Add Host Header enabled. Its API key is encrypted at
                    rest and never shown again.
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
                            placeholder="Enter API key"
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
    </div>
  )
}

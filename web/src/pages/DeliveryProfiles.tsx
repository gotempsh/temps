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
import { Alert, AlertDescription } from '@/components/ui/alert'
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
import { Cloud, Globe, Plus, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  name: z.string().trim().min(1, 'Name this profile').max(100),
  provider_kind: z.enum(['direct', 'cloudflare']),
})
type ProfileForm = z.infer<typeof schema>

export default function DeliveryProfiles() {
  usePageTitle('Delivery profiles')
  const { setBreadcrumbs } = useBreadcrumbs()
  const client = useQueryClient()
  const [open, setOpen] = useState(false)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const profiles = useQuery({
    queryKey: ['delivery-profiles'],
    queryFn: async () => requireDeliveryData(await listDeliveryProfiles()),
  })
  const form = useForm<ProfileForm>({
    resolver: zodResolver(schema),
    defaultValues: { name: '', provider_kind: 'direct' },
  })
  const capabilities = useQuery({
    queryKey: ['delivery-capabilities'],
    queryFn: async () => requireDeliveryData(await getDeliveryCapabilities()),
  })
  const create = useMutation({
    mutationFn: async (body: ProfileForm) =>
      requireDeliveryData(await createDeliveryProfile({ body })),
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
            Choose how traffic reaches your applications. Reuse a profile across
            projects, or give each project its own delivery provider.
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
                  <Cloud className="mt-1 size-5 shrink-0" />
                ) : (
                  <Globe className="mt-1 size-5 shrink-0" />
                )}
                <div className="min-w-0">
                  <p className="break-words font-medium">{profile.name}</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {profile.provider_kind === 'cloudflare'
                      ? 'Cloudflare proxy'
                      : 'Direct to origin'}
                  </p>
                </div>
              </div>
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
            </li>
          ))}
        </ul>
      ) : (
        <div className="rounded-lg border border-dashed px-6 py-12 text-center">
          <Globe className="mx-auto mb-4 size-7 text-muted-foreground" />
          <h2 className="font-medium">Set up your first delivery profile</h2>
          <p className="mx-auto mt-2 max-w-md text-sm text-muted-foreground">
            For example, use Direct for your API and Cloudflare for your
            storefront. Creating a profile does not change any existing domain.
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
                      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
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
                            className={`rounded-lg border p-4 text-left transition-colors ${field.value === capability.provider_kind ? 'border-primary bg-primary/5 ring-1 ring-primary' : 'hover:bg-muted'}`}
                          >
                            <span className="font-medium">
                              {capability.name}
                            </span>
                            <span className="mt-2 block text-xs leading-relaxed text-muted-foreground">
                              {capability.requirements.join('. ')}
                            </span>
                            {!capability.configured && (
                              <span className="mt-2 block text-xs font-medium">
                                Setup required
                              </span>
                            )}
                          </button>
                        ))}
                      </div>
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
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
                    !capability.configured && capability.setup_path
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

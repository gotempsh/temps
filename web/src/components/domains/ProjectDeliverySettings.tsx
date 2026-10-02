// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironments,
  getProjectDeliverySettings,
  updateProjectDeliverySettings,
} from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { ChevronDown } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { deliveryError, requireDeliveryData } from './delivery-errors'
import {
  deliveryProfilePickerQueryKey,
  deliveryProfileQueryKey,
  fetchDeliveryProfile,
  fetchDeliveryProfilePicker,
  includeDeliveryProfile,
  isDeliveryProfileListTruncated,
  overridesForProviderChoice,
  unlistedOverrideProfileIds,
  type DeliveryProfileOption,
} from './delivery-queries'
import {
  DeliveryProfileLimitNote,
  DeliveryProfileSelect,
} from './DeliveryProfileSelect'
import { DeliveryProviderChoice } from './DeliveryProviderChoice'
import type { DeliveryProviderChoiceValue } from './DeliveryProviderChoice'

const defaultsSchema = z.object({
  project: z.string(),
  environments: z.record(z.string(), z.string()),
})
type DefaultsForm = z.infer<typeof defaultsSchema>

export function ProjectDeliverySettings({ projectId }: { projectId: number }) {
  const client = useQueryClient()
  const profiles = useQuery({
    queryKey: deliveryProfilePickerQueryKey,
    queryFn: fetchDeliveryProfilePicker,
  })
  const environments = useQuery({
    queryKey: ['delivery-environments', projectId],
    queryFn: async () =>
      requireDeliveryData(
        await getEnvironments({ path: { project_id: projectId } })
      ),
  })
  const settings = useQuery({
    queryKey: ['delivery-settings', projectId],
    queryFn: async () =>
      requireDeliveryData(
        await getProjectDeliverySettings({ path: { project_id: projectId } })
      ),
  })
  const form = useForm<DefaultsForm>({
    resolver: zodResolver(defaultsSchema),
    defaultValues: { project: '', environments: {} },
  })
  const hasMultipleEnvironments = (environments.data?.length ?? 0) > 1
  const overrideCount =
    environments.data?.filter((environment) =>
      settings.data?.environment_overrides.some(
        (override) =>
          override.environment_id === environment.id &&
          override.profile_id != null
      )
    ).length ?? 0
  useEffect(() => {
    if (settings.data && environments.data)
      form.reset({
        project:
          settings.data.default_profile_id == null
            ? ''
            : String(settings.data.default_profile_id),
        environments: Object.fromEntries(
          environments.data.map((environment) => {
            const profileId = settings.data.environment_overrides.find(
              (row) => row.environment_id === environment.id
            )?.profile_id
            return [
              `env_${environment.id}`,
              profileId == null ? '' : String(profileId),
            ]
          })
        ),
      })
  }, [settings.data, environments.data, form])
  const save = useMutation({
    mutationFn: async (values: DefaultsForm) =>
      requireDeliveryData(
        await updateProjectDeliverySettings({
          path: { project_id: projectId },
          body: {
            default_profile_id: values.project ? Number(values.project) : null,
            environment_overrides: (environments.data ?? []).map(
              (environment) => ({
                environment_id: environment.id,
                profile_id:
                  hasMultipleEnvironments &&
                  values.environments[`env_${environment.id}`]
                    ? Number(values.environments[`env_${environment.id}`])
                    : null,
              })
            ),
          },
        })
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ['delivery-settings', projectId] })
      toast.success('Delivery defaults saved')
    },
  })
  // Overrides can use profiles outside the first page by name. Load those by
  // ID, so the pickers can name them and a provider switch knows their kind.
  const overrideProfiles = useQueries({
    queries: unlistedOverrideProfileIds(
      settings.data?.environment_overrides ?? [],
      profiles.data?.items ?? []
    ).map((profileId) => ({
      queryKey: deliveryProfileQueryKey(profileId),
      queryFn: () => fetchDeliveryProfile(profileId),
    })),
  })
  // The first page of profiles by name, plus the project default and the
  // override profiles that fall outside that page.
  const profileOptions = [
    settings.data?.effective_default_profile,
    ...overrideProfiles.map((query) => query.data),
  ].reduce<DeliveryProfileOption[]>(
    (options, extra) => includeDeliveryProfile(options, extra),
    profiles.data?.items ?? []
  )
  const profilesTruncated =
    profiles.data !== undefined && isDeliveryProfileListTruncated(profiles.data)
  const cloudflareProfile = profileOptions.find(
    (profile) => profile.provider_kind === 'cloudflare'
  )
  const bunnyProfile = profileOptions.find(
    (profile) => profile.provider_kind === 'bunny'
  )
  const selectedProvider: DeliveryProviderChoiceValue =
    settings.data?.effective_default_profile?.provider_kind === 'cloudflare'
      ? 'cloudflare'
      : settings.data?.effective_default_profile?.provider_kind === 'bunny'
        ? 'bunny'
        : 'none'
  const setProvider = useMutation({
    mutationFn: async (selected: DeliveryProviderChoiceValue) => {
      const chosenProfile =
        selected === 'cloudflare'
          ? cloudflareProfile
          : selected === 'bunny'
            ? bunnyProfile
            : undefined
      if (selected !== 'none' && !chosenProfile)
        throw new Error(`Create a ${selected} delivery profile first`)
      const environmentOverrides = overridesForProviderChoice(
        settings.data?.environment_overrides ?? [],
        selected,
        hasMultipleEnvironments,
        (profileId) =>
          profileOptions.find((profile) => profile.id === profileId)
            ?.provider_kind
      )
      // Never clear an override without knowing whether it pins a CDN.
      if ('unknownProfileId' in environmentOverrides)
        throw new Error(
          `Delivery profile #${environmentOverrides.unknownProfileId}, used by an environment override, could not be loaded. Reload the page and try again.`
        )
      return requireDeliveryData(
        await updateProjectDeliverySettings({
          path: { project_id: projectId },
          body: {
            default_profile_id: chosenProfile?.id ?? null,
            environment_overrides: environmentOverrides.overrides,
          },
        })
      )
    },
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ['delivery-settings', projectId] })
      toast.success('Delivery provider updated for this project')
    },
    onError: (error: Error) => toast.error(deliveryError(error)),
  })
  const chooseProvider = (selected: DeliveryProviderChoiceValue) => {
    if (selected === selectedProvider) return
    const candidates =
      selected === 'none'
        ? []
        : profileOptions.filter((profile) => profile.provider_kind === selected)
    if (candidates.length > 1 || (profilesTruncated && candidates.length > 0)) {
      // Never guess between profiles of the same provider: the user picks
      // the exact one in the Project default field below. A partial list
      // may hide more of them, so it is never taken as the only one.
      const label = selected === 'cloudflare' ? 'Cloudflare' : 'Bunny'
      toast.info(
        profilesTruncated
          ? `Not every profile is listed here, so there may be several ${label} profiles. Choose one in Project default, then save.`
          : `You have ${candidates.length} ${label} profiles. Choose one in Project default, then save.`
      )
      form.setFocus('project')
      return
    }
    setProvider.mutate(selected)
  }
  const pending =
    profiles.isPending || environments.isPending || settings.isPending
  const error = profiles.error ?? environments.error ?? settings.error
  return (
    <section
      className="space-y-5 border-b pb-6"
      aria-labelledby="delivery-defaults-title"
    >
      <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-start">
        <div>
          <h3 id="delivery-defaults-title" className="font-semibold">
            Delivery defaults
          </h3>
          <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
            Choose how new domain setups reach this project. Existing bindings
            keep their applied configuration until you preview and apply a
            change.
          </p>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link to="/delivery-profiles">Manage profiles</Link>
        </Button>
      </div>
      <div className="space-y-3">
        <div>
          <p className="font-medium">Delivery provider</p>
          <p className="text-sm text-muted-foreground">
            Sets the project default for new domain setups. Existing bindings
            stay as configured.
          </p>
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            <Link className="text-sm underline" to="/dns-providers">
              Manage DNS providers
            </Link>
            {!cloudflareProfile && (
              <Link className="text-sm underline" to="/delivery-profiles">
                Create a Cloudflare profile
              </Link>
            )}
            {!bunnyProfile && (
              <Link className="text-sm underline" to="/delivery-profiles">
                Create a Bunny profile
              </Link>
            )}
          </div>
        </div>
        <DeliveryProviderChoice
          value={selectedProvider}
          onChange={chooseProvider}
          cloudflareConfigured={!!cloudflareProfile}
          bunnyConfigured={!!bunnyProfile}
          disabled={pending || !!error || setProvider.isPending}
        />
        <DeliveryProfileLimitNote listing={profiles.data} />
      </div>
      {pending ? (
        <Skeleton className="h-24 w-full" />
      ) : error ? (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(error)}{' '}
            <Button
              variant="link"
              onClick={() => {
                profiles.refetch()
                environments.refetch()
                settings.refetch()
              }}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : profileOptions.length === 0 ? (
        <div className="bg-muted/40 p-4 text-sm">
          Create a delivery profile to configure managed traffic for this
          project.{' '}
          <Link
            className="font-medium underline underline-offset-4"
            to="/delivery-profiles"
          >
            Set up a profile
          </Link>
        </div>
      ) : (
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit((values) => save.mutate(values))}
            className="space-y-4 border-t pt-5"
          >
            <FormField
              control={form.control}
              name="project"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Project default</FormLabel>
                  <FormControl>
                    <DeliveryProfileSelect
                      profiles={profileOptions}
                      value={field.value}
                      onChange={field.onChange}
                      inheritLabel="No managed delivery default"
                      disabled={save.isPending}
                      triggerRef={field.ref}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            {hasMultipleEnvironments && (
              <Collapsible className="group border-t pt-5">
                <CollapsibleTrigger asChild>
                  <button
                    type="button"
                    className="flex w-full items-start justify-between gap-4 rounded-md text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <span className="space-y-1">
                      <span className="block font-medium">
                        Environment overrides
                      </span>
                      <span className="block text-sm text-muted-foreground">
                        Override the project default for a specific environment.
                        {overrideCount > 0 && ` ${overrideCount} configured.`}
                      </span>
                    </span>
                    <ChevronDown className="mt-1 size-4 shrink-0 text-muted-foreground transition-transform group-data-[state=open]:rotate-180" />
                  </button>
                </CollapsibleTrigger>
                <CollapsibleContent className="pt-4">
                  <div className="grid gap-4 sm:grid-cols-2">
                    {environments.data?.map((environment) => (
                      <FormField
                        key={environment.id}
                        control={form.control}
                        name={`environments.env_${environment.id}`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>{environment.name}</FormLabel>
                            <FormControl>
                              <DeliveryProfileSelect
                                profiles={profileOptions}
                                value={field.value ?? ''}
                                onChange={field.onChange}
                                inheritLabel="Inherit project default"
                                disabled={save.isPending}
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    ))}
                  </div>
                </CollapsibleContent>
              </Collapsible>
            )}
            {save.isError && (
              <Alert variant="destructive">
                <AlertDescription>{deliveryError(save.error)}</AlertDescription>
              </Alert>
            )}
            <Button
              type="submit"
              variant="outline"
              disabled={save.isPending || !form.formState.isDirty}
            >
              {save.isPending ? 'Saving…' : 'Save defaults'}
            </Button>
          </form>
        </Form>
      )}
    </section>
  )
}

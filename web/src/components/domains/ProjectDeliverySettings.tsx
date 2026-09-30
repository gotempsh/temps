// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironments,
  getProjectDeliverySettings,
  listDeliveryProfiles,
  updateProjectDeliverySettings,
  type DeliveryProfileResponse,
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type Ref, useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { ChevronDown } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { deliveryError, requireDeliveryData } from './delivery-errors'
import { DeliveryProviderChoice } from './DeliveryProviderChoice'
import type { DeliveryProviderChoiceValue } from './DeliveryProviderChoice'

const defaultsSchema = z.object({
  project: z.string(),
  environments: z.record(z.string(), z.string()),
})
type DefaultsForm = z.infer<typeof defaultsSchema>
const INHERIT_PROFILE = 'inherit-profile'

export function DeliveryProfileSelect({
  profiles,
  value,
  onChange,
  inheritLabel,
  disabled,
  id,
  'aria-describedby': ariaDescribedBy,
  'aria-invalid': ariaInvalid,
  triggerRef,
}: {
  profiles: DeliveryProfileResponse[]
  value: string
  onChange: (value: string) => void
  inheritLabel: string
  disabled?: boolean
  id?: string
  'aria-describedby'?: string
  'aria-invalid'?: boolean
  triggerRef?: Ref<HTMLButtonElement>
}) {
  return (
    <Select
      value={value || INHERIT_PROFILE}
      onValueChange={(selected) =>
        onChange(selected === INHERIT_PROFILE ? '' : selected)
      }
      disabled={disabled}
    >
      <SelectTrigger
        ref={triggerRef}
        id={id}
        aria-describedby={ariaDescribedBy}
        aria-invalid={ariaInvalid}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={INHERIT_PROFILE}>{inheritLabel}</SelectItem>
        {profiles.map((profile) => (
          <SelectItem key={profile.id} value={String(profile.id)}>
            {profile.name} (
            {profile.provider_kind === 'direct'
              ? 'Direct'
              : profile.provider_kind === 'bunny'
                ? 'Bunny'
                : 'Cloudflare'}
            )
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

export function ProjectDeliverySettings({ projectId }: { projectId: number }) {
  const client = useQueryClient()
  const profiles = useQuery({
    queryKey: ['delivery-profiles'],
    queryFn: async () => requireDeliveryData(await listDeliveryProfiles()),
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
  const cloudflareProfile = profiles.data?.find(
    (profile) => profile.provider_kind === 'cloudflare'
  )
  const bunnyProfile = profiles.data?.find(
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
      return requireDeliveryData(
        await updateProjectDeliverySettings({
          path: { project_id: projectId },
          body: {
            default_profile_id: chosenProfile?.id ?? null,
            environment_overrides: (
              settings.data?.environment_overrides ?? []
            ).map((override) => ({
              ...override,
              profile_id:
                hasMultipleEnvironments &&
                (selected !== 'none' ||
                  profiles.data?.some(
                    (profile) =>
                      profile.id === override.profile_id &&
                      profile.provider_kind === 'direct'
                  ))
                  ? override.profile_id
                  : null,
            })),
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
        : (profiles.data ?? []).filter(
            (profile) => profile.provider_kind === selected
          )
    if (candidates.length > 1) {
      // Never guess between profiles of the same provider: the user picks
      // the exact one in the Project default field below.
      const label = selected === 'cloudflare' ? 'Cloudflare' : 'Bunny'
      toast.info(
        `You have ${candidates.length} ${label} profiles. Choose one in Project default, then save.`
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
      ) : !profiles.data?.length ? (
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
                      profiles={profiles.data ?? []}
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
                                profiles={profiles.data ?? []}
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

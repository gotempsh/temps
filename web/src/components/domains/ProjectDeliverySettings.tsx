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
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'
import { deliveryError, requireDeliveryData } from './delivery-errors'

const defaultsSchema = z.object({
  project: z.string(),
  environments: z.record(z.string(), z.string()),
})
type DefaultsForm = z.infer<typeof defaultsSchema>

export function DeliveryProfileSelect({
  profiles,
  value,
  onChange,
  inheritLabel,
  disabled,
  id,
}: {
  profiles: DeliveryProfileResponse[]
  value: string
  onChange: (value: string) => void
  inheritLabel: string
  disabled?: boolean
  id?: string
}) {
  return (
    <select
      id={id}
      className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
      value={value}
      onChange={(event) => onChange(event.target.value)}
      disabled={disabled}
    >
      <option value="">{inheritLabel}</option>
      {profiles.map((profile) => (
        <option key={profile.id} value={String(profile.id)}>
          {profile.name} (
          {profile.provider_kind === 'direct' ? 'Direct' : 'Cloudflare'})
        </option>
      ))}
    </select>
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
                profile_id: values.environments[`env_${environment.id}`]
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
  const pending =
    profiles.isPending || environments.isPending || settings.isPending
  const error = profiles.error ?? environments.error ?? settings.error
  return (
    <section
      className="rounded-lg border p-4 sm:p-5"
      aria-labelledby="delivery-defaults-title"
    >
      <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-start">
        <div>
          <h3 id="delivery-defaults-title" className="font-semibold">
            Delivery defaults
          </h3>
          <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
            Choose how new domain setups reach this project. Environment
            overrides take priority. Existing bindings keep their applied
            configuration until you preview and apply a change.
          </p>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link to="/delivery-profiles">Manage profiles</Link>
        </Button>
      </div>
      {pending ? (
        <Skeleton className="mt-5 h-24 w-full" />
      ) : error ? (
        <Alert className="mt-4" variant="destructive">
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
        <div className="mt-5 rounded-md bg-muted/40 p-4 text-sm">
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
            className="mt-5 space-y-4"
          >
            <div className="grid gap-4 sm:grid-cols-2">
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
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
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

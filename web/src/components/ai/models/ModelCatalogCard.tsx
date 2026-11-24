// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { refreshModelCatalog, updateModelCatalogSettings } from '@/api/client'
import type { ModelCatalogStatus } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  type CatalogSettingsForm,
  catalogFormToUpdate,
  catalogSettingsSchema,
  catalogSettingsToForm,
  catalogSourceLabel,
} from '@/lib/ai-model-catalog'
import { extractProblemDetails } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { Loader2, Pencil, RefreshCw, TriangleAlert } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { MODEL_CATALOG_QUERY_KEY } from './query-keys'

function problemMessage(error: unknown, fallback: string): string {
  const problem = extractProblemDetails(error)
  return problem?.detail ?? problem?.title ?? fallback
}

function ago(value: string | null | undefined): string {
  if (!value) return 'never'
  return formatDistanceToNow(new Date(value), { addSuffix: true })
}

function Fact({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm font-medium [overflow-wrap:anywhere]">
        {children}
      </dd>
    </div>
  )
}

export function ModelCatalogCardSkeleton() {
  return (
    <Card role="status" aria-label="Loading the model catalog">
      <CardHeader>
        <Skeleton className="h-5 w-40" />
        <Skeleton className="h-4 w-64" />
      </CardHeader>
      <CardContent className="grid grid-cols-2 gap-4 md:grid-cols-4">
        {[0, 1, 2, 3].map((index) => (
          <div key={index} className="space-y-1">
            <Skeleton className="h-3 w-20" />
            <Skeleton className="h-4 w-24" />
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

/**
 * Where model prices and newer models come from, when they were last
 * refreshed, and "Refresh now".
 */
export function ModelCatalogCard({ status }: { status: ModelCatalogStatus }) {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState(false)
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: MODEL_CATALOG_QUERY_KEY })

  const refresh = useMutation({
    mutationFn: async () => {
      const { data } = await refreshModelCatalog({ throwOnError: true })
      return data
    },
    onSuccess: (result) => {
      toast.success(
        `Catalog refreshed: ${result.stored} models, ${result.applied_prices} prices applied` +
          (result.skipped > 0 ? `, ${result.skipped} rejected` : '')
      )
      void invalidate()
    },
    onError: (error) => {
      toast.error(problemMessage(error, 'The catalog could not be refreshed'))
      void invalidate()
    },
  })

  const settings = status.settings
  const refreshing = refresh.isPending || status.refreshing

  return (
    <Card>
      <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <CardTitle>Model catalog</CardTitle>
          <CardDescription>
            Prices and models published after this release.
          </CardDescription>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
            <Pencil className="size-4" />
            <span className="max-sm:sr-only">Settings</span>
          </Button>
          <Button
            size="sm"
            onClick={() => refresh.mutate()}
            disabled={refreshing}
          >
            {refreshing ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <RefreshCw className="size-4" />
            )}
            Refresh now
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {status.last_error ? (
          <Alert variant="destructive">
            <TriangleAlert className="size-4" />
            <AlertTitle>The last refresh failed</AlertTitle>
            <AlertDescription className="[overflow-wrap:anywhere]">
              {status.last_error} The previous catalog and the bundled prices
              stay in use.
            </AlertDescription>
          </Alert>
        ) : null}
        <dl className="grid grid-cols-2 gap-4 md:grid-cols-4">
          <Fact label="Source">
            {catalogSourceLabel(settings)}
            {settings.source_url ? null : (
              <Badge variant="secondary" className="ml-2">
                default
              </Badge>
            )}
          </Fact>
          <Fact label="Last refreshed">{ago(status.last_refreshed_at)}</Fact>
          <Fact label="Models in catalog">
            {status.entry_count}
            {status.skipped_count > 0
              ? ` (${status.skipped_count} rejected)`
              : ''}
          </Fact>
          <Fact label="Prices applied">
            {status.applied_price_count} over {status.bundled_model_count}{' '}
            bundled
          </Fact>
          <Fact label="Next refresh">
            {settings.auto_refresh
              ? status.next_refresh_at
                ? formatDistanceToNow(new Date(status.next_refresh_at), {
                    addSuffix: true,
                  })
                : 'due now'
              : 'off'}
          </Fact>
          <Fact label="Interval">
            every {settings.refresh_interval_hours} h
          </Fact>
          <Fact label="Provider model discovery">
            {settings.discover_provider_models
              ? `on, last ${ago(status.last_discovery_at)}`
              : 'off'}
          </Fact>
          <Fact label="Last attempt">{ago(status.last_attempt_at)}</Fact>
        </dl>
      </CardContent>
      <CatalogSettingsDialog
        open={editing}
        onOpenChange={setEditing}
        status={status}
        onSaved={() => {
          setEditing(false)
          void invalidate()
        }}
      />
    </Card>
  )
}

function CatalogSettingsDialog({
  open,
  onOpenChange,
  status,
  onSaved,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  status: ModelCatalogStatus
  onSaved: () => void
}) {
  const initial = catalogSettingsToForm(status.settings)
  const form = useForm<CatalogSettingsForm>({
    resolver: zodResolver(catalogSettingsSchema),
    defaultValues: initial,
  })
  const {
    sourceUrl,
    refreshIntervalHours,
    autoRefresh,
    discoverProviderModels,
  } = initial
  useEffect(() => {
    if (open) {
      form.reset({
        sourceUrl,
        refreshIntervalHours,
        autoRefresh,
        discoverProviderModels,
      })
    }
  }, [
    open,
    sourceUrl,
    refreshIntervalHours,
    autoRefresh,
    discoverProviderModels,
    form,
  ])

  const save = useMutation({
    mutationFn: async (values: CatalogSettingsForm) => {
      await updateModelCatalogSettings({
        body: catalogFormToUpdate(values),
        throwOnError: true,
      })
    },
    onSuccess: () => {
      toast.success('Catalog settings saved')
      onSaved()
    },
    onError: (error) =>
      toast.error(
        problemMessage(error, 'The catalog settings could not be saved')
      ),
  })

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => !save.isPending && onOpenChange(next)}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Catalog settings</DialogTitle>
          <DialogDescription>
            Changing the catalog changes the prices every project’s AI usage is
            estimated at.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit((values) => save.mutate(values))}
            className="space-y-4"
          >
            <FormField
              control={form.control}
              name="sourceUrl"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Catalog URL</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      inputMode="url"
                      placeholder={status.settings.default_source_url}
                    />
                  </FormControl>
                  <FormDescription>
                    A models.dev-format JSON document over https. Leave blank
                    for the catalog published with Temps.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="autoRefresh"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between gap-4">
                  <FormLabel>Refresh automatically</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="refreshIntervalHours"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Hours between refreshes</FormLabel>
                  <FormControl>
                    <Input {...field} inputMode="numeric" />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="discoverProviderModels"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between gap-4">
                  <div className="space-y-1">
                    <FormLabel>Discover provider models</FormLabel>
                    <FormDescription>
                      Ask each provider key for its models on the same schedule.
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={save.isPending}
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={save.isPending}>
                {save.isPending ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : null}
                Save
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

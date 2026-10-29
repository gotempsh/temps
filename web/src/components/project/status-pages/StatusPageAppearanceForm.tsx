// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { updateStatusPageMutation } from '@/api/client/@tanstack/react-query.gen'
import type { StatusPageResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
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
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Laptop, Moon, Sun } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import {
  appearanceDefaults,
  appearanceFormSchema,
  DEFAULT_ACCENT_COLOR,
  normalizeAccent,
  operationQueries,
  type AppearanceFormValues,
} from './status-page-form'

const THEMES = [
  {
    value: 'auto',
    label: 'Match visitor',
    hint: 'Light or dark, following each visitor’s system setting',
    icon: Laptop,
  },
  { value: 'light', label: 'Light', hint: 'Always light', icon: Sun },
  { value: 'dark', label: 'Dark', hint: 'Always dark', icon: Moon },
] as const

/** Brand colors offered as one-click choices. */
const SWATCHES = [
  '#4f46e5',
  '#2563eb',
  '#0891b2',
  '#16a34a',
  '#ca8a04',
  '#ea580c',
  '#db2777',
  '#18181b',
]

/** Title, description and look of the public page. */
export function StatusPageAppearanceForm({
  page,
}: {
  page: StatusPageResponse
}) {
  const queryClient = useQueryClient()
  const form = useForm<AppearanceFormValues>({
    resolver: zodResolver(appearanceFormSchema),
    defaultValues: appearanceDefaults(page),
  })
  const accentColor = useWatch({ control: form.control, name: 'accent_color' })
  useEffect(() => {
    form.reset(appearanceDefaults(page), { keepDirtyValues: true })
  }, [form, page])

  const update = useMutation({
    ...updateStatusPageMutation(),
    meta: { errorTitle: 'Failed to save appearance' },
    onSuccess: async (saved) => {
      form.reset(appearanceDefaults(saved))
      await queryClient.invalidateQueries(operationQueries('getStatusPage'))
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
      toast.success(
        saved.is_published
          ? 'Saved. The public page is updated.'
          : 'Saved. Check the preview, then publish.'
      )
    },
  })

  const onSubmit = (values: AppearanceFormValues) => {
    update.mutate({
      path: { status_page_id: page.id },
      body: {
        title: values.title,
        description: values.description,
        appearance: {
          theme: values.theme,
          accent_color: normalizeAccent(values.accent_color),
          logo_url: values.logo_url.trim(),
          website_url: values.website_url.trim(),
          show_uptime_percentage: values.show_uptime_percentage,
          show_response_times: values.show_response_times,
        },
      },
    })
  }

  const accent = normalizeAccent(accentColor)
  const pickerValue = /^#[0-9a-f]{6}$/.test(accent)
    ? accent
    : DEFAULT_ACCENT_COLOR

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)}>
        <fieldset
          className="min-w-0 space-y-8"
          disabled={update.isPending}
          aria-busy={update.isPending}
        >
          <section className="space-y-4">
            <h3 className="text-lg font-semibold">Header</h3>
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Page title</FormLabel>
                  <FormControl>
                    <Input {...field} />
                  </FormControl>
                  <FormDescription>
                    Shown in the header (when there is no logo) and the browser
                    tab.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="description"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Description (optional)</FormLabel>
                  <FormControl>
                    <Textarea
                      rows={2}
                      placeholder="Live status of the Acme API and dashboard."
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <FormField
                control={form.control}
                name="logo_url"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Logo URL (optional)</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="https://example.com/logo.svg"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      An https:// image, at most 40px tall on the page.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="website_url"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Website link (optional)</FormLabel>
                    <FormControl>
                      <Input placeholder="https://example.com" {...field} />
                    </FormControl>
                    <FormDescription>
                      Adds a “Visit website” button to the header.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </section>

          <section className="space-y-4">
            <h3 className="text-lg font-semibold">Look</h3>
            <FormField
              control={form.control}
              name="theme"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Theme</FormLabel>
                  <div
                    role="radiogroup"
                    aria-label="Theme"
                    className="grid grid-cols-1 gap-2 sm:grid-cols-3"
                  >
                    {THEMES.map((theme) => (
                      <button
                        key={theme.value}
                        type="button"
                        role="radio"
                        aria-checked={field.value === theme.value}
                        onClick={() => field.onChange(theme.value)}
                        className={cn(
                          'flex items-start gap-3 rounded-lg border p-3 text-left transition-colors hover:bg-muted/50',
                          field.value === theme.value &&
                            'border-primary bg-primary/5'
                        )}
                      >
                        <theme.icon className="mt-0.5 size-4 shrink-0" />
                        <span>
                          <span className="block text-sm font-medium">
                            {theme.label}
                          </span>
                          <span className="block text-xs text-muted-foreground">
                            {theme.hint}
                          </span>
                        </span>
                      </button>
                    ))}
                  </div>
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="accent_color"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Brand color</FormLabel>
                  <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
                    <div className="flex items-center gap-2">
                      <input
                        type="color"
                        aria-label="Pick a brand color"
                        value={pickerValue}
                        onChange={(e) => field.onChange(e.target.value)}
                        className="h-9 w-12 cursor-pointer rounded border bg-transparent"
                      />
                      <FormControl>
                        <Input
                          className="w-full sm:w-32"
                          placeholder={DEFAULT_ACCENT_COLOR}
                          {...field}
                        />
                      </FormControl>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {SWATCHES.map((color) => (
                        <button
                          key={color}
                          type="button"
                          aria-label={`Use ${color}`}
                          onClick={() => field.onChange(color)}
                          className={cn(
                            'size-7 rounded-full border-2 border-transparent',
                            accent === color && 'border-foreground'
                          )}
                          style={{ backgroundColor: color }}
                        />
                      ))}
                    </div>
                  </div>
                  <FormDescription>
                    Used for the header stripe, links and the response-time
                    chart. Status colors (green, orange, red) stay the same so
                    visitors can always read them. Leave empty for the default.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
          </section>

          <section className="space-y-3">
            <h3 className="text-lg font-semibold">What to show</h3>
            <FormField
              control={form.control}
              name="show_uptime_percentage"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between gap-4 space-y-0 rounded-lg border p-3">
                  <div>
                    <FormLabel>Uptime percentages</FormLabel>
                    <FormDescription>
                      e.g. “99.98% uptime” under each component, and per day on
                      hover. The colored bars are always shown.
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
            <FormField
              control={form.control}
              name="show_response_times"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between gap-4 space-y-0 rounded-lg border p-3">
                  <div>
                    <FormLabel>Response times</FormLabel>
                    <FormDescription>
                      Average milliseconds per day on hover, and a 90-day
                      response-time chart under each component.
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
          </section>

          <div className="flex justify-end">
            <Button type="submit" disabled={update.isPending}>
              {update.isPending ? 'Saving...' : 'Save appearance'}
            </Button>
          </div>
        </fieldset>
      </form>
    </Form>
  )
}

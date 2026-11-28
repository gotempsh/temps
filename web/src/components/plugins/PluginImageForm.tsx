// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { DialogFooter } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import type { useAddPluginFromImage } from '@/hooks/usePlugins'
import {
  PLUGIN_IMAGE_STEPS,
  imagePinnedByDigest,
  pluginImageCommand,
  pluginServiceName,
  type PluginImageProgress,
  type PluginImageStep,
} from '@/lib/plugin-image'
import { addServiceFailure } from '@/lib/plugin-services'
import { zodResolver } from '@hookform/resolvers/zod'
import { AlertCircle, Check, Circle, Loader2 } from 'lucide-react'
import { useEffect, useId } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  image: z
    .string()
    .trim()
    .min(1, "Enter the plugin's image.")
    .regex(/^\S+$/, 'An image reference has no spaces.'),
  name: z
    .string()
    .trim()
    .min(1, 'Name the service the plugin runs as.')
    .max(63, 'A service name has at most 63 characters.')
    .regex(
      /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/,
      'Use lowercase letters, digits and hyphens.'
    ),
  port: z
    .number({ message: 'Enter the port the plugin listens on.' })
    .int('A port is a whole number.')
    .min(1, 'A port is between 1 and 65535.')
    .max(65535, 'A port is between 1 and 65535.'),
})

type Values = z.infer<typeof schema>

/** An image and the plugin it belongs to, to start the form with. */
export interface PluginImageChoice {
  image: string
  /** The plugin's name, which its service is named after. */
  name?: string
}

/**
 * Add a plugin from its image in one click: the service is created, the
 * image deployed and the service added, with each step shown as it happens.
 * A step that fails says why and is tried again from where it stopped.
 */
export function PluginImageForm({
  open,
  configured,
  initial,
  flow,
  onSensitiveError,
  onAdded,
  onCancel,
}: {
  /** Whether the dialog this form is in is open: it starts over when opened. */
  open: boolean
  /** Whether this server can run plugins as services. */
  configured: boolean
  initial?: PluginImageChoice
  flow: ReturnType<typeof useAddPluginFromImage>
  onSensitiveError: (error: unknown, retry: () => void) => boolean
  onAdded: (name: string) => void
  onCancel: () => void
}) {
  const id = useId()
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { image: '', name: '', port: 8080 },
  })
  const { reset, setValue, register } = form
  const resetFlow = flow.reset
  const initialImage = initial?.image ?? ''
  const initialName = initial?.name ?? ''
  useEffect(() => {
    if (!open) return
    reset({
      image: initialImage,
      name: initialName || pluginServiceName(initialImage),
      port: 8080,
    })
    resetFlow()
  }, [open, initialImage, initialName, reset, resetFlow])

  const image = useWatch({ control: form.control, name: 'image' })
  const errors = form.formState.errors
  const nameEdited = form.formState.dirtyFields.name === true

  async function submit(values: Values) {
    try {
      const { added } = await flow.mutateAsync(values)
      toast.success(
        `${added.display_name ?? added.name} is now a plugin. It can use Temps only as far as you allow.`
      )
      onCancel()
      onAdded(added.name)
    } catch (error) {
      // Any other failure is shown by the app-wide mutation error handler
      // with the reason, and repeated below the steps.
      onSensitiveError(error, () => void submit(values))
    }
  }

  return (
    <form className="space-y-4" onSubmit={form.handleSubmit(submit)}>
      <fieldset disabled={!configured || flow.isPending} className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor={`${id}-image`}>Image</Label>
          <Input
            id={`${id}-image`}
            autoComplete="off"
            spellCheck={false}
            placeholder="registry.example.com/acme/reports:1.2.0"
            aria-invalid={errors.image ? true : undefined}
            {...register('image', {
              onChange: (event: React.ChangeEvent<HTMLInputElement>) => {
                // The service is named after the image until it is named by hand.
                if (!nameEdited)
                  setValue('name', pluginServiceName(event.target.value), {
                    shouldValidate: form.formState.isSubmitted,
                  })
              },
            })}
          />
          {errors.image ? (
            <p role="alert" className="text-sm text-destructive">
              {errors.image.message}
            </p>
          ) : (
            image.trim() !== '' &&
            !imagePinnedByDigest(image) && (
              <p className="text-xs text-muted-foreground">
                A tag can be moved to other content. Temps records the content
                it deploys; name the image by digest to deploy exactly what you
                reviewed.
              </p>
            )
          )}
        </div>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-[1fr_8rem]">
          <div className="space-y-2">
            <Label htmlFor={`${id}-name`}>Service name</Label>
            <Input
              id={`${id}-name`}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={errors.name ? true : undefined}
              {...register('name')}
            />
            {errors.name && (
              <p role="alert" className="text-sm text-destructive">
                {errors.name.message}
              </p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-port`}>Port</Label>
            <Input
              id={`${id}-port`}
              type="number"
              inputMode="numeric"
              min={1}
              max={65535}
              aria-invalid={errors.port ? true : undefined}
              {...register('port', { valueAsNumber: true })}
            />
            {errors.port && (
              <p role="alert" className="text-sm text-destructive">
                {errors.port.message}
              </p>
            )}
          </div>
        </div>
      </fieldset>
      <PluginImageSteps
        at={flow.at?.step ?? null}
        running={flow.isPending}
        failed={flow.isError}
      />
      {flow.isError && (
        <PluginImageFailure
          error={flow.error}
          progress={flow.at?.progress ?? {}}
        />
      )}
      <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
        <span className="shrink-0">From a terminal:</span>
        <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1 [overflow-wrap:anywhere]">
          {pluginImageCommand(image)}
        </code>
        <CopyButton
          value={pluginImageCommand(image)}
          label="Copy the command that adds this plugin from a terminal"
          className="h-8 shrink-0 rounded-md border bg-background px-3"
        >
          Copy command
        </CopyButton>
      </div>
      <DialogFooter>
        <Button
          type="button"
          variant="outline"
          disabled={flow.isPending}
          onClick={onCancel}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={!configured || flow.isPending}>
          {flow.isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
          {flow.isPending
            ? 'Working…'
            : flow.isError
              ? 'Try again'
              : 'Add plugin'}
        </Button>
      </DialogFooter>
    </form>
  )
}

/**
 * The steps of adding a plugin from an image: which are done, which one runs
 * and which one failed. Nothing before the first run.
 */
export function PluginImageSteps({
  at,
  running,
  failed,
}: {
  at: PluginImageStep | null
  running: boolean
  failed: boolean
}) {
  if (at === null) return null
  const current = PLUGIN_IMAGE_STEPS.findIndex(({ step }) => step === at)
  // Not running and not failed: the last step finished too.
  const finished = !running && !failed
  return (
    <ol role="status" aria-label="Progress" className="space-y-1 text-sm">
      {PLUGIN_IMAGE_STEPS.map(({ step, label }, index) => {
        const done = finished || index < current
        const state = done
          ? 'Done'
          : index > current
            ? 'Not started'
            : failed
              ? 'Failed'
              : 'In progress'
        return (
          <li key={step} className="flex items-center gap-2">
            {done ? (
              <Check className="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
            ) : index > current ? (
              <Circle className="size-4 shrink-0 text-muted-foreground/50" />
            ) : failed ? (
              <AlertCircle className="size-4 shrink-0 text-destructive" />
            ) : (
              <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
            )}
            <span className={index > current ? 'text-muted-foreground' : ''}>
              {label}
            </span>
            <span className="sr-only">: {state}</span>
          </li>
        )
      })}
    </ol>
  )
}

/**
 * Why a step failed and what it left behind: a service that was created
 * stays, and is linked so its logs can be read.
 */
export function PluginImageFailure({
  error,
  progress,
}: {
  error: unknown
  progress: PluginImageProgress
}) {
  return (
    <div role="alert" className="space-y-1 text-sm">
      <p className="break-words text-destructive">{addServiceFailure(error)}</p>
      {progress.projectSlug && (
        <p className="text-muted-foreground">
          The service{' '}
          <Link
            to={`/projects/${progress.projectSlug}`}
            className="font-medium text-foreground underline"
          >
            {progress.projectSlug}
          </Link>{' '}
          was created and is kept. Try again continues with it.
        </p>
      )}
    </div>
  )
}

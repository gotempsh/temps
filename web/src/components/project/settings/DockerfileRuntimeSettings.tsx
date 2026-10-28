// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import {
  containerRuntimeSchema,
  parseRuntimeArguments,
} from '@/lib/container-runtime-overrides'
import { zodResolver } from '@hookform/resolvers/zod'
import { Button, Field, FormErrors, SettingsGroup } from '@temps-sdk/ds'
import { Textarea } from '@temps-sdk/ui'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

interface DockerfileRuntimeSettingsProps {
  project: ProjectResponse
  saving: boolean
  onSave: (overrides: {
    preset_config: Record<string, unknown>
  }) => Promise<void>
}

export function DockerfileRuntimeSettings(
  props: DockerfileRuntimeSettingsProps
) {
  return <DockerfileRuntimeForm key={props.project.id} {...props} />
}

function DockerfileRuntimeForm({
  project,
  saving,
  onSave,
}: DockerfileRuntimeSettingsProps) {
  const config = (project.preset_config ?? {}) as Record<string, unknown>
  const form = useForm({
    resolver: zodResolver(containerRuntimeSchema),
    resetOptions: { keepDirtyValues: true },
    values: {
      command: config.command == null ? '' : JSON.stringify(config.command),
      entrypoint:
        config.entrypoint == null ? '' : JSON.stringify(config.entrypoint),
    },
  })
  const busy = saving || form.formState.isSubmitting
  const errors = {
    Command: form.formState.errors.command?.message,
    Entrypoint: form.formState.errors.entrypoint?.message,
    Save: form.formState.errors.root?.message,
  }
  return (
    <SettingsGroup
      title="Container startup"
      description="Run a different process from the same Dockerfile image, such as a background worker. Changes apply on the next deployment."
    >
      <form
        className="space-y-5"
        onSubmit={form.handleSubmit(async (values) => {
          if (saving) return
          try {
            await onSave({
              preset_config: {
                ...config,
                command: parseRuntimeArguments(values.command),
                entrypoint: parseRuntimeArguments(values.entrypoint),
              },
            })
            form.reset(values)
            toast.success(
              'Container startup settings saved. Deploy to apply them.'
            )
          } catch {
            form.setError('root', {
              message:
                'Could not save container startup settings. Your edits have been kept; try again.',
            })
          }
        })}
      >
        <Field
          label="Command (CMD)"
          optional
          description='A JSON argument list, for example ["python", "-m", "worker"]. Leave blank to inherit the image command.'
          error={errors.Command}
        >
          {(props) => (
            <Textarea
              {...props}
              {...form.register('command')}
              readOnly={busy}
              className="font-mono"
              placeholder='["python", "-m", "worker"]'
            />
          )}
        </Field>
        <Field
          label="Entrypoint (ENTRYPOINT)"
          optional
          description='Leave blank to inherit the image entrypoint, or enter [] to remove it. An override replaces the image startup program; Command is inherited unless you override it too. For shell syntax, use ["/bin/sh", "-c"] with a single command string in Command.'
          error={errors.Entrypoint}
        >
          {(props) => (
            <Textarea
              {...props}
              {...form.register('entrypoint')}
              readOnly={busy}
              className="font-mono"
              placeholder='["/app/entrypoint.sh"]'
            />
          )}
        </Field>
        <FormErrors errors={errors} />
        <div className="flex justify-end border-t pt-3">
          <Button
            type="submit"
            busy={busy}
            busyLabel="Saving…"
            aria-disabled={!form.formState.isDirty}
            className={
              !form.formState.isDirty
                ? 'pointer-events-none opacity-50'
                : undefined
            }
            onClick={(event) => {
              if (!form.formState.isDirty) event.preventDefault()
            }}
          >
            Save startup settings
          </Button>
        </div>
      </form>
    </SettingsGroup>
  )
}

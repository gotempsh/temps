// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import {
  containerNameSchema,
  type ContainerNameFormValues,
} from './container-name'

/** The part of `GET /kv/settings` and `GET /blob/settings` this field shows. */
export interface ContainerNameSettings {
  container_name: string
  default_container_name: string
  custom: boolean
  in_use: boolean
  required_prefix: string
}

interface ContainerNameFieldProps {
  /** Name of the service in prose, e.g. "KV Store". */
  serviceName: string
  settings?: ContainerNameSettings
  isLoading: boolean
  error?: unknown
  canManage: boolean
  isSaving: boolean
  /**
   * Save a new name; an empty string restores the default. `onSaved` must be
   * called once the save succeeded, so a failed save keeps the form open.
   */
  onSave: (containerName: string, onSaved: () => void) => void
  onRetry: () => void
}

interface ContainerNameFormProps {
  settings: ContainerNameSettings
  /** Whether the form is shown. It stays mounted so its state is kept. */
  open: boolean
  isSaving: boolean
  onSave: (containerName: string) => void
  onCancel: () => void
}

function ContainerNameForm({
  settings,
  open,
  isSaving,
  onSave,
  onCancel,
}: ContainerNameFormProps) {
  const form = useForm<ContainerNameFormValues>({
    resolver: zodResolver(containerNameSchema(settings.required_prefix)),
    defaultValues: { container_name: settings.container_name },
  })

  return (
    <Form {...form}>
      <form
        className="space-y-3"
        hidden={!open}
        onSubmit={form.handleSubmit((values) => {
          const name = values.container_name.trim()
          // Submitting the derived default unchanged restores the default
          // rather than pinning it as a custom name.
          onSave(name === settings.default_container_name ? '' : name)
        })}
      >
        <FormField
          control={form.control}
          name="container_name"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Container name</FormLabel>
              <FormControl>
                <Input
                  {...field}
                  className="font-mono"
                  autoComplete="off"
                  spellCheck={false}
                />
              </FormControl>
              <FormDescription>
                Must start with <code>{settings.required_prefix}</code>. Give
                each Temps instance that shares this Docker host its own name.
                Leave empty for the default{' '}
                <code>{settings.default_container_name}</code>.
              </FormDescription>
              <FormMessage />
            </FormItem>
          )}
        />
        <div className="flex flex-wrap gap-2">
          <Button type="submit" size="sm" disabled={isSaving}>
            {isSaving ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Saving...
              </>
            ) : (
              'Save name'
            )}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => {
              form.reset({ container_name: settings.container_name })
              onCancel()
            }}
            disabled={isSaving}
          >
            Cancel
          </Button>
        </div>
      </form>
    </Form>
  )
}

/**
 * The Docker container a managed service runs in on this instance, with a way
 * to change it before the service exists. Always rendered, enabled or not, so
 * an operator on a shared Docker host finds the setting before hitting a
 * name conflict.
 */
export function ContainerNameField({
  serviceName,
  settings,
  isLoading,
  error,
  canManage,
  isSaving,
  onSave,
  onRetry,
}: ContainerNameFieldProps) {
  const [editing, setEditing] = useState(false)

  if (isLoading) {
    return <Skeleton className="h-16 w-full" />
  }

  if (error != null || !settings) {
    return (
      <div className="rounded-lg border bg-muted/30 p-3">
        <p className="text-xs text-muted-foreground">Docker container</p>
        <p className="mt-0.5 text-sm text-muted-foreground">
          The container name for {serviceName} could not be read.
        </p>
        <Button
          variant="link"
          size="sm"
          className="h-auto p-0"
          onClick={onRetry}
        >
          Retry
        </Button>
      </div>
    )
  }

  const canEdit = canManage && !settings.in_use

  return (
    <div className="space-y-2 rounded-lg border bg-muted/30 p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">Docker container</p>
          <p className="mt-0.5 truncate font-mono text-sm font-medium">
            {settings.container_name}
          </p>
        </div>
        {canEdit && !editing && (
          <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
            Change name
          </Button>
        )}
      </div>
      <p className="text-xs text-muted-foreground">
        {settings.custom
          ? 'Custom name for this instance.'
          : 'Default name for this instance.'}{' '}
        {settings.in_use
          ? 'It cannot change while the service exists; delete the service first to pick another name.'
          : `If another Temps instance on this Docker host already uses it, enabling ${serviceName} fails with a conflict. Pick a unique name first.`}
      </p>
      {canEdit && (
        <ContainerNameForm
          key={settings.container_name}
          settings={settings}
          open={editing}
          isSaving={isSaving}
          onSave={(name) => onSave(name, () => setEditing(false))}
          onCancel={() => setEditing(false)}
        />
      )}
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Field } from '@temps-sdk/ds'
import { Controller, useForm } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import { client } from '@/api/client/client.gen'
import { installLocalFile } from '@/api/client/sdk.gen'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'

const schema = z.object({
  name: z
    .string()
    .regex(
      /^[a-z0-9][a-z0-9-]{0,63}$/,
      'Enter the name declared by the plugin.'
    ),
  version: z.string().min(1, 'Enter the manifest version.').max(64),
  file: z.custom<File>(
    (value) =>
      typeof File !== 'undefined' &&
      value instanceof File &&
      value.size > 0 &&
      value.size <= 256 * 1024 * 1024,
    'Choose a non-empty executable of at most 256 MiB.'
  ),
  trusted: z
    .boolean()
    .refine((value) => value, 'Confirm that you trust this executable.'),
})

export function LocalFileInstall({
  open,
  onOpenChange,
  expectedName,
  onSensitiveError,
}: {
  expectedName: string
  open: boolean
  onOpenChange: (open: boolean) => void
  onSensitiveError: (error: unknown, retry: () => void) => boolean
}) {
  const queries = useQueryClient()
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: { name: '', version: '', trusted: false },
  })
  const digest = useMutation({
    gcTime: 0,
    retry: false,
    mutationFn: async (file: File) => {
      const valid = schema.shape.file.safeParse(file)
      if (!valid.success) throw new Error(valid.error.issues[0].message)
      if (!globalThis.crypto?.subtle)
        throw new Error(
          'Use HTTPS or localhost to verify the file digest, or use temps plugin install-local.'
        )
      const bytes = await crypto.subtle.digest(
        'SHA-256',
        await file.arrayBuffer()
      )
      return Array.from(new Uint8Array(bytes), (byte) =>
        byte.toString(16).padStart(2, '0')
      ).join('')
    },
  })
  const { reset } = form
  const resetDigest = digest.reset
  useEffect(() => {
    resetDigest()
    reset({ name: expectedName, version: '', trusted: false })
  }, [expectedName, open, reset, resetDigest])
  const baseUrl = client.getConfig().baseUrl ?? '/api'
  const destination =
    open && typeof document !== 'undefined'
      ? new URL(baseUrl, document.baseURI).href
      : baseUrl
  const install = useMutation({
    mutationFn: async ({
      file,
      name,
      version,
      sha256,
      target,
    }: z.infer<typeof schema> & { sha256: string; target: string }) => {
      return (
        await installLocalFile({
          baseUrl: target,
          query: { name, version },
          headers: { 'X-Temps-Trust-Sha256': sha256 },
          body: { file },
          throwOnError: true,
        })
      ).data
    },
    retry: false,
    onSuccess: async (result) => {
      toast.success(`Installed ${result.name}@${result.version}`)
      form.reset()
      onOpenChange(false)
      await queries.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
  async function submit(values: z.infer<typeof schema>) {
    if (!digest.data || digest.variables !== values.file) return
    try {
      await install.mutateAsync({
        ...values,
        sha256: digest.data,
        target: destination,
      })
    } catch (error) {
      onSensitiveError(error, () => void submit(values))
    }
  }
  const errors = form.formState.errors
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (install.isPending) return
        install.reset()
        form.reset()
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Install from file</DialogTitle>
          <DialogDescription>
            Choose a native executable built for the Temps host. Its manifest
            must match the name and version below.
          </DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={form.handleSubmit(submit)}>
          <fieldset disabled={install.isPending} className="space-y-4">
            <Controller
              control={form.control}
              name="file"
              render={({ field }) => (
                <Field label="Executable" error={errors.file?.message}>
                  {(props) => (
                    <Input
                      {...props}
                      type="file"
                      onChange={(event) => {
                        form.resetField('trusted')
                        const file = event.target.files?.[0]
                        field.onChange(file)
                        if (file) digest.mutate(file)
                        else digest.reset()
                      }}
                    />
                  )}
                </Field>
              )}
            />
            <Field label="Plugin name" error={errors.name?.message}>
              {(props) => (
                <Input
                  {...props}
                  readOnly={Boolean(expectedName)}
                  {...form.register('name')}
                />
              )}
            </Field>
            <Field label="Manifest version" error={errors.version?.message}>
              {(props) => <Input {...props} {...form.register('version')} />}
            </Field>
            <div className="space-y-1 break-all text-sm">
              <p>
                Install destination <code>{destination}</code>
              </p>
              {digest.isPending && <p role="status">Computing SHA-256…</p>}
              {digest.data && (
                <p>
                  SHA-256 <code>{digest.data}</code>
                </p>
              )}
              {digest.error && (
                <p role="alert" className="text-destructive">
                  {digest.error.message}
                </p>
              )}
            </div>
            <Controller
              control={form.control}
              name="trusted"
              render={({ field }) => (
                <Field
                  label="I trust this executable to run native code on the Temps host"
                  error={errors.trusted?.message}
                >
                  {(props) => (
                    <Checkbox
                      {...props}
                      disabled={!digest.isSuccess}
                      checked={field.value}
                      onCheckedChange={(value) =>
                        field.onChange(value === true)
                      }
                    />
                  )}
                </Field>
              )}
            />
          </fieldset>
          {install.error && (
            <p role="alert" className="text-sm text-destructive">
              {sensitiveActionErrorMessage(
                install.error,
                'Installation failed. Check the plugin status before retrying.'
              )}
            </p>
          )}
          <Button
            type="submit"
            disabled={install.isPending || !digest.isSuccess}
          >
            {install.isPending ? 'Installing…' : 'Install plugin'}
          </Button>
        </form>
      </DialogContent>
    </Dialog>
  )
}

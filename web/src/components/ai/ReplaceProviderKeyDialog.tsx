// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

const schema = z.object({
  apiKey: z.string().trim().min(1, 'Paste the new API key'),
})

type FormData = z.infer<typeof schema>

/**
 * Swap the secret behind a provider key, keeping everything set on it: its
 * name, models, default model and endpoint. The old value is never shown;
 * the new one is stored encrypted and is not shown again either.
 */
export function ReplaceProviderKeyDialog({
  keyName,
  providerName,
  open,
  pending,
  error,
  onOpenChange,
  onReplace,
}: {
  keyName: string
  providerName: string
  open: boolean
  pending: boolean
  /** Why the last attempt was refused, e.g. the provider rejected the key. */
  error: string | null
  onOpenChange: (open: boolean) => void
  /** Resolves once the new key is saved; a rejection keeps the dialog open. */
  onReplace: (apiKey: string) => Promise<unknown>
}) {
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors },
  } = useForm<FormData>({
    resolver: zodResolver(schema),
    defaultValues: { apiKey: '' },
  })

  const close = (next: boolean) => {
    // Closing mid-save would hide whether the new key was stored.
    if (pending) return
    if (!next) reset()
    onOpenChange(next)
  }

  const submit = handleSubmit(async ({ apiKey }) => {
    try {
      await onReplace(apiKey)
      reset()
    } catch {
      // The caller reports the failure; keep what was typed for a retry.
    }
  })

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="sm:max-w-md">
        <form onSubmit={submit} className="space-y-4">
          <DialogHeader>
            <DialogTitle>Replace API key</DialogTitle>
            <DialogDescription>
              Use a new {providerName} key for &ldquo;{keyName}&rdquo;. Its
              models, default model and endpoint stay as they are. Requests use
              the new key as soon as it is saved.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="replacement-api-key">New API key</Label>
            <Input
              id="replacement-api-key"
              type="password"
              autoComplete="off"
              spellCheck={false}
              placeholder="sk-..."
              aria-invalid={errors.apiKey ? true : undefined}
              {...register('apiKey')}
            />
            {errors.apiKey && (
              <p className="text-sm text-destructive">
                {errors.apiKey.message}
              </p>
            )}
            {error && !errors.apiKey && (
              <p role="alert" className="text-sm text-destructive">
                {error}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => close(false)}
              disabled={pending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2 className="mr-2 size-4 animate-spin" />}
              Replace key
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

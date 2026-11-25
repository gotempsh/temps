// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getGitProviderQueryKey,
  updateGitProviderCredentialsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Info, RefreshCw } from 'lucide-react'
import { problemDetail } from '@/lib/api-problem'
import {
  credentialFieldErrors,
  fieldsForProvider,
  type Props,
  type UpdateProviderCredentialsBody,
} from './CredentialsEditorDialog-shared'

export function CredentialsEditorDialog({
  provider,
  open,
  onOpenChange,
}: Props) {
  const queryClient = useQueryClient()
  const fields = fieldsForProvider(provider)
  const [values, setValues] = useState<Record<string, string>>({})
  const [wasOpen, setWasOpen] = useState(open)

  if (open !== wasOpen) {
    setWasOpen(open)
    if (open) setValues({})
  }

  const mutation = useMutation({
    ...updateGitProviderCredentialsMutation(),
    onSuccess: () => {
      toast.success('Credentials updated')
      setValues({})
      queryClient.invalidateQueries({
        queryKey: getGitProviderQueryKey({
          path: { provider_id: provider.id },
        }),
      })
      onOpenChange(false)
    },
    onError: (error) => {
      toast.error('Failed to update credentials', {
        description: problemDetail(error, 'Check the values and try again.'),
      })
    },
  })

  const handleOpenChange = (next: boolean) => {
    // Reopening starts from a clean form, without the last attempt's error.
    if (!next) mutation.reset()
    onOpenChange(next)
  }

  if (fields.length === 0) {
    return null
  }

  const hasChanges = Object.values(values).some((v) => v.trim().length > 0)
  const fieldErrors = credentialFieldErrors(fields, values)
  const hasErrors = Object.keys(fieldErrors).length > 0

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    const body: UpdateProviderCredentialsBody = {}
    for (const field of fields) {
      const v = values[field.key as string]
      if (v && v.trim().length > 0) {
        ;(body as Record<string, string>)[field.key as string] = v
      }
    }
    if (Object.keys(body).length === 0) {
      toast.info('No changes to save')
      return
    }
    if (hasErrors) return
    mutation.mutate({ path: { provider_id: provider.id }, body })
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Edit credentials</DialogTitle>
          <DialogDescription>
            Replace any field below. Fields left blank keep their current value.
            Stored secrets are never displayed.
          </DialogDescription>
        </DialogHeader>

        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription className="text-sm">
            After updating credentials, existing connections may need to be
            re-authorized for the new values to take effect.
          </AlertDescription>
        </Alert>

        <form onSubmit={handleSubmit} className="space-y-4">
          {fields.map((field) => {
            const id = `cred-${field.key}`
            const value = values[field.key as string] ?? ''
            const onChange = (v: string) => {
              // A failed save stays on screen until the user changes a value.
              if (mutation.isError) mutation.reset()
              setValues((prev) => ({ ...prev, [field.key as string]: v }))
            }
            const error = fieldErrors[field.key]
            const errorId = `${id}-error`

            return (
              <div key={field.key as string}>
                <Label htmlFor={id}>{field.label}</Label>
                {field.type === 'textarea' ? (
                  <textarea
                    id={id}
                    value={value}
                    onChange={(e) => onChange(e.target.value)}
                    placeholder={field.placeholder}
                    rows={6}
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? errorId : undefined}
                    className="mt-1 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-1 focus:ring-ring"
                  />
                ) : (
                  <Input
                    id={id}
                    type={field.type}
                    value={value}
                    onChange={(e) => onChange(e.target.value)}
                    placeholder={field.placeholder}
                    className="mt-1 font-mono"
                    autoComplete="off"
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? errorId : undefined}
                  />
                )}
                {error && (
                  <p id={errorId} className="mt-1 text-xs text-destructive">
                    {error}
                  </p>
                )}
                {field.help && (
                  <p className="text-xs text-muted-foreground mt-1">
                    {field.help}
                  </p>
                )}
              </div>
            )
          })}

          {mutation.isError && (
            <p role="alert" className="text-sm text-destructive">
              Credentials were not saved:{' '}
              {problemDetail(mutation.error, 'Check the values and try again.')}
            </p>
          )}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => handleOpenChange(false)}
              disabled={mutation.isPending}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={!hasChanges || hasErrors || mutation.isPending}
            >
              {mutation.isPending ? (
                <>
                  <RefreshCw className="mr-2 h-4 w-4 animate-spin" />
                  Saving…
                </>
              ) : (
                'Save Credentials'
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

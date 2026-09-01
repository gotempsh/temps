// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { getGitProviderQueryKey } from '@/api/client/@tanstack/react-query.gen'
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
import type { ProviderResponse } from '@/api/client/types.gen'
import {
  updateGitProviderCredentials,
  type UpdateProviderCredentialsBody,
} from '@/lib/git-providers'

interface FieldSpec {
  key: keyof UpdateProviderCredentialsBody
  label: string
  type: 'text' | 'password' | 'textarea'
  placeholder: string
  help?: string
}

function fieldsForProvider(provider: ProviderResponse): FieldSpec[] {
  const type = provider.provider_type
  const method = provider.auth_method

  if (type === 'github' && (method === 'app' || method === 'github_app')) {
    return [
      {
        key: 'app_id',
        label: 'App ID',
        type: 'text',
        placeholder: '123456',
        help: 'Integer App ID from your GitHub App settings.',
      },
      {
        key: 'client_id',
        label: 'Client ID',
        type: 'text',
        placeholder: 'Iv1.abc…',
      },
      {
        key: 'client_secret',
        label: 'Client Secret',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
      },
      {
        key: 'private_key',
        label: 'Private Key (PEM)',
        type: 'textarea',
        placeholder: 'Leave blank to keep current value',
        help: 'Paste the full PEM including BEGIN/END lines.',
      },
      {
        key: 'webhook_secret',
        label: 'Webhook Secret',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
      },
    ]
  }

  if (type === 'gitlab' && method === 'oauth') {
    return [
      {
        key: 'client_id',
        label: 'Application ID (Client ID)',
        type: 'text',
        placeholder: 'From your GitLab application details',
      },
      {
        key: 'client_secret',
        label: 'Secret (Client Secret)',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
      },
    ]
  }

  if (type === 'gitlab' && method === 'gitlab_app') {
    return [
      {
        key: 'app_id',
        label: 'Application ID',
        type: 'text',
        placeholder: 'From GitLab application details',
      },
      {
        key: 'app_secret',
        label: 'Secret',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
      },
    ]
  }

  if (method === 'pat' || method === 'token') {
    return [
      {
        key: 'token',
        label: 'Personal Access Token',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
        help:
          type === 'github'
            ? 'GitHub PAT with `repo` scope.'
            : 'GitLab PAT with `api`, `read_repository`, and `write_repository` scopes.',
      },
    ]
  }

  return []
}

/** True when the provider's auth method has editable fields. Use to gate
 *  whether to show the "Edit Credentials" action in the UI. */
export function providerHasEditableCredentials(
  provider: ProviderResponse,
): boolean {
  return fieldsForProvider(provider).length > 0
}

interface Props {
  provider: ProviderResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function CredentialsEditorDialog({
  provider,
  open,
  onOpenChange,
}: Props) {
  const queryClient = useQueryClient()
  const fields = fieldsForProvider(provider)
  const [values, setValues] = useState<Record<string, string>>({})

  // Reset inputs whenever the dialog (re)opens so secrets never leak between sessions.
  useEffect(() => {
    if (open) setValues({})
  }, [open])

  const mutation = useMutation({
    mutationFn: (body: UpdateProviderCredentialsBody) =>
      updateGitProviderCredentials(provider.id, body),
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
    onError: (err: Error) => {
      toast.error(`Failed to update credentials: ${err.message}`)
    },
  })

  if (fields.length === 0) {
    return null
  }

  const hasChanges = Object.values(values).some((v) => v.trim().length > 0)

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
    mutation.mutate(body)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Edit Credentials</DialogTitle>
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
            const onChange = (v: string) =>
              setValues((prev) => ({ ...prev, [field.key as string]: v }))

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
                  />
                )}
                {field.help && (
                  <p className="text-xs text-muted-foreground mt-1">
                    {field.help}
                  </p>
                )}
              </div>
            )
          })}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={mutation.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!hasChanges || mutation.isPending}>
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

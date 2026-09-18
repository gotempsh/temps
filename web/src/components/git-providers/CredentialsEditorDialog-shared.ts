// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProviderResponse } from '@/api/client/types.gen'
import { type UpdateProviderCredentialsBody } from '@/lib/git-providers'

export interface FieldSpec {
  key: keyof UpdateProviderCredentialsBody
  label: string
  type: 'text' | 'password' | 'textarea'
  placeholder: string
  help?: string
}

export function fieldsForProvider(provider: ProviderResponse): FieldSpec[] {
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
// Pure capability helper intentionally shared with the provider detail page.
export function providerHasEditableCredentials(
  provider: ProviderResponse
): boolean {
  return fieldsForProvider(provider).length > 0
}

export interface Props {
  provider: ProviderResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}

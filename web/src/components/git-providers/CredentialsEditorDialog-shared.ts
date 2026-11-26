// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProviderResponse,
  UpdateProviderCredentialsRequest,
} from '@/api/client/types.gen'

/** The credential fields the editor can send (the generated request body). */
export type UpdateProviderCredentialsBody = UpdateProviderCredentialsRequest

export interface FieldSpec {
  key: keyof UpdateProviderCredentialsBody
  label: string
  type: 'text' | 'password' | 'textarea'
  placeholder: string
  help?: string
  /** A field that must be replaced together with this one. */
  pairedWith?: keyof UpdateProviderCredentialsBody
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
        label: 'Client secret',
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
        label: 'Webhook secret',
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
        label: 'Personal access token',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
        help:
          type === 'github'
            ? 'GitHub PAT with `repo` scope.'
            : 'GitLab PAT with `api`, `read_repository`, and `write_repository` scopes.',
      },
    ]
  }

  if (method === 'basic') {
    return [
      {
        key: 'username',
        label: 'Username',
        type: 'text',
        placeholder: 'Leave blank to keep current value',
      },
      {
        key: 'password',
        label: type === 'bitbucket' ? 'App password' : 'Password or token',
        type: 'password',
        placeholder: 'Leave blank to keep current value',
      },
    ]
  }

  if (method === 'ssh') {
    return [
      {
        key: 'private_key',
        label: 'Private key',
        type: 'textarea',
        placeholder: 'Leave blank to keep current value',
        help: 'Paste the full key including BEGIN/END lines. Replace it together with the public key.',
        pairedWith: 'public_key',
      },
      {
        key: 'public_key',
        label: 'Public key',
        type: 'textarea',
        placeholder: 'ssh-ed25519 AAAA… deploy@example',
        pairedWith: 'private_key',
      },
    ]
  }

  return []
}

/**
 * Errors to show beside fields before Save: a paired field (an SSH key pair)
 * left blank while its partner is filled in, which the server would refuse.
 * Keyed by field; empty when the values can be submitted.
 */
export function credentialFieldErrors(
  fields: FieldSpec[],
  values: Record<string, string | undefined>
): Partial<Record<keyof UpdateProviderCredentialsBody, string>> {
  const filled = (key: string) => (values[key] ?? '').trim().length > 0
  const errors: Partial<Record<keyof UpdateProviderCredentialsBody, string>> =
    {}
  for (const field of fields) {
    const partner = fields.find((other) => other.key === field.pairedWith)
    if (partner && !filled(field.key) && filled(partner.key)) {
      errors[field.key] =
        `Required: the ${partner.label.toLowerCase()} is replaced together with the ${field.label.toLowerCase()}.`
    }
  }
  return errors
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

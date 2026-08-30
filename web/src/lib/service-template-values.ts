// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  generateAppUrl,
  type DeploymentUrlBase,
} from '@/components/templates/envVarGenerators'

export interface ServiceTemplateValueInput {
  name: string
  kind: string
  defaultValue?: string | null
  routeService?: string | null
  routeIsPrimary?: boolean
}

function randomBytes(length: number): Uint8Array {
  if (typeof crypto === 'undefined' || !crypto.getRandomValues) {
    throw new Error('Web Crypto is required to generate service credentials')
  }
  const bytes = new Uint8Array(length)
  crypto.getRandomValues(bytes)
  return bytes
}

function randomFromAlphabet(length: number, alphabet: string): string {
  const unbiasedLimit = 256 - (256 % alphabet.length)
  let value = ''
  while (value.length < length) {
    for (const byte of randomBytes(Math.max(32, length - value.length))) {
      if (byte < unbiasedLimit) value += alphabet[byte % alphabet.length]
      if (value.length === length) break
    }
  }
  return value
}

function randomAlphaNumeric(length: number, lowercase = false): string {
  return randomFromAlphabet(
    length,
    lowercase
      ? 'abcdefghijklmnopqrstuvwxyz0123456789'
      : 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
  )
}

function randomPasswordWithSymbols(length: number): string {
  return randomFromAlphabet(
    length,
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+[]{}'
  )
}

function randomHex(byteLength: number): string {
  return Array.from(randomBytes(byteLength), (byte) =>
    byte.toString(16).padStart(2, '0')
  ).join('')
}

function randomBase64(byteLength: number): string {
  let binary = ''
  for (const byte of randomBytes(byteLength)) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary)
}

function publicHost(projectSlug: string, base?: DeploymentUrlBase): string {
  const url = serviceRouteUrl(projectSlug, undefined, true, base)
  if (!url) return ''
  try {
    return new URL(url).hostname
  } catch {
    return ''
  }
}

function serviceRouteUrl(
  projectSlug: string,
  service: string | null | undefined,
  primary: boolean,
  base?: DeploymentUrlBase
): string {
  if (primary || !service) {
    return generateAppUrl({ repositoryName: projectSlug, base }) || ''
  }
  const resolved = base || {
    scheme: 'https' as const,
    host: 'temps.sh',
  }
  const portPart = resolved.port ? `:${resolved.port}` : ''
  return `${resolved.scheme}://${service}--${projectSlug}-production.${resolved.host}${portPart}`
}

/** Generate the initial value for one typed Coolify template variable. */
export function generateServiceTemplateValue(
  input: ServiceTemplateValueInput,
  projectSlug: string,
  base?: DeploymentUrlBase
): string {
  switch (input.kind) {
    case 'public_url': {
      const url = serviceRouteUrl(
        projectSlug,
        input.routeService,
        input.routeIsPrimary !== false,
        base
      )
      if (input.defaultValue?.startsWith('/')) {
        return `${url.replace(/\/$/, '')}${input.defaultValue}`
      }
      return input.defaultValue || url
    }
    case 'public_host': {
      try {
        const host = new URL(
          serviceRouteUrl(
            projectSlug,
            input.routeService,
            input.routeIsPrimary !== false,
            base
          )
        ).hostname
        if (input.defaultValue?.startsWith('/')) {
          return `${host}${input.defaultValue}`
        }
        return input.defaultValue || host
      } catch {
        return publicHost(projectSlug, base)
      }
    }
  }
  if (input.defaultValue != null) return input.defaultValue
  switch (input.kind) {
    case 'generated_password':
      return randomAlphaNumeric(32)
    case 'generated_password_64':
      return randomAlphaNumeric(64)
    case 'generated_password_with_symbols':
      return randomPasswordWithSymbols(32)
    case 'generated_password_with_symbols_64':
      return randomPasswordWithSymbols(64)
    case 'generated_user':
      return randomAlphaNumeric(16)
    case 'generated_lowercase_user':
      return randomAlphaNumeric(16, true)
    case 'generated_random_32':
      return randomAlphaNumeric(32)
    case 'generated_random_64':
      return randomAlphaNumeric(64)
    case 'generated_random_128':
      return randomAlphaNumeric(128)
    case 'generated_base64_32':
      return randomBase64(32)
    case 'generated_base64_64':
      return randomBase64(64)
    case 'generated_base64_128':
      return randomBase64(128)
    case 'generated_hex_32':
      return randomHex(16)
    case 'generated_hex_64':
      return randomHex(32)
    case 'generated_hex_128':
      return randomHex(64)
    default:
      return ''
  }
}

function base64Url(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function base64UrlJson(value: object): string {
  return base64Url(new TextEncoder().encode(JSON.stringify(value)))
}

async function generateSupabaseJwt(
  signingKey: string,
  role: 'anon' | 'service_role',
  now = new Date()
): Promise<string> {
  if (!crypto?.subtle) {
    throw new Error('Web Crypto is required to generate Supabase credentials')
  }
  const issuedAt = Math.floor(now.getTime() / 60_000) * 60
  const expiresAt = new Date(now)
  expiresAt.setUTCFullYear(expiresAt.getUTCFullYear() + 100)
  const header = base64UrlJson({ alg: 'HS256', typ: 'JWT' })
  const payload = base64UrlJson({
    iss: 'supabase',
    iat: issuedAt,
    exp: Math.floor(expiresAt.getTime() / 1000),
    role,
  })
  const unsigned = `${header}.${payload}`
  const key = await crypto.subtle.importKey(
    'raw',
    new TextEncoder().encode(signingKey),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  )
  const signature = await crypto.subtle.sign(
    'HMAC',
    key,
    new TextEncoder().encode(unsigned)
  )
  return `${unsigned}.${base64Url(new Uint8Array(signature))}`
}

/** Resolve a generator whose value depends on another template variable. */
export async function generateDependentServiceTemplateValue(
  kind: string,
  values: Record<string, string>
): Promise<string | null> {
  const signingKey = values.SERVICE_PASSWORD_JWT
  if (!signingKey) return null
  switch (kind) {
    case 'generated_supabase_anon':
      return generateSupabaseJwt(signingKey, 'anon')
    case 'generated_supabase_service':
      return generateSupabaseJwt(signingKey, 'service_role')
    default:
      return null
  }
}

export function serviceTemplateVariableIsGenerated(kind: string): boolean {
  return kind !== 'user_input'
}

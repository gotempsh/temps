// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Well-known env vars that need a random locally-generated secret (auth
// session/signing keys, encryption keys) rather than a value copied from
// somewhere else. Encoded as hex by default — that's what standalone
// generators like better-auth-secret.com actually produce for every length
// tier (their own "-base64 32" label doesn't match their real output), and
// hex has the advantage of never needing shell/URL escaping. Curated
// overrides here exist for names that don't fit the generic
// *_SECRET/*_KEY suffix heuristic below, or that need a non-default
// length/encoding/prefix.
export interface GeneratedSecretSpec {
  bytes: number
  encoding: 'base64' | 'hex'
  /** Literal prefix the framework expects on the value itself, e.g. Laravel's `base64:`. */
  prefix?: string
}

const KNOWN_SECRET_VAR_SPECS: Record<string, GeneratedSecretSpec> = {
  BETTER_AUTH_SECRET: { bytes: 32, encoding: 'hex' },
  NEXTAUTH_SECRET: { bytes: 32, encoding: 'hex' },
  AUTH_SECRET: { bytes: 32, encoding: 'hex' },
  SECRET_KEY_BASE: { bytes: 64, encoding: 'hex' }, // Rails
  // Laravel's artisan key:generate always writes a base64: prefix followed by
  // base64 content — this one has to stay base64, unlike the others above.
  APP_KEY: { bytes: 32, encoding: 'base64', prefix: 'base64:' },
  DJANGO_SECRET_KEY: { bytes: 32, encoding: 'hex' },
  FLASK_SECRET_KEY: { bytes: 32, encoding: 'hex' },
  PAYLOAD_SECRET: { bytes: 32, encoding: 'hex' }, // Payload CMS
  NUXT_SESSION_PASSWORD: { bytes: 32, encoding: 'hex' }, // nuxt-auth-utils
  LUCIA_AUTH_SECRET: { bytes: 32, encoding: 'hex' },
}

// *_SECRET / *_KEY / *_PASSWORD vars are almost always locally-generated
// signing/encryption material or app credentials — EXCEPT when they belong to
// one of these well-known third-party providers, where the real value has to
// come from that provider's dashboard. Generating a random value for e.g.
// STRIPE_SECRET_KEY would look configured but silently fail every API call,
// so those are excluded from the generic heuristic below.
const EXTERNAL_PROVIDER_PREFIXES = [
  'STRIPE', 'AWS', 'GITHUB', 'GITLAB', 'BITBUCKET', 'GOOGLE', 'OPENAI',
  'ANTHROPIC', 'AZURE', 'GCP', 'SENDGRID', 'TWILIO', 'MAILGUN', 'POSTMARK',
  'RESEND', 'CLOUDFLARE', 'SUPABASE', 'FIREBASE', 'PLAID', 'SLACK', 'DISCORD',
  'ALGOLIA', 'PUSHER', 'SENTRY', 'POSTHOG', 'VERCEL', 'NETLIFY', 'PADDLE',
  'LEMONSQUEEZY', 'RAZORPAY', 'PAYPAL', 'SQUARE', 'DIGITALOCEAN', 'LINEAR',
  'NOTION', 'INTERCOM', 'SEGMENT', 'MIXPANEL', 'AMPLITUDE', 'HUBSPOT',
  'SALESFORCE', 'ZENDESK', 'SHOPIFY', 'RECAPTCHA', 'TURNSTILE', 'HCAPTCHA',
  'MAPBOX', 'ONESIGNAL', 'CLERK', 'AUTH0', 'OKTA', 'WORKOS',
]

// *_PASSWORD vars for a database/service connection have to match that
// service's *actual* password (set via "fill from service", or whatever the
// service was provisioned with) — a random value would just disconnect it.
// This is a narrower, exact-name exclusion rather than a prefix list since
// "password" is too generic a suffix to safely prefix-match.
const SERVICE_CONNECTION_PASSWORD_NAMES = new Set([
  'DATABASE_PASSWORD',
  'DB_PASSWORD',
  'POSTGRES_PASSWORD',
  'POSTGRESQL_PASSWORD',
  'PGPASSWORD',
  'MYSQL_PASSWORD',
  'MARIADB_PASSWORD',
  'MONGO_PASSWORD',
  'MONGODB_PASSWORD',
  'REDIS_PASSWORD',
])

/** Selectable secret lengths, mirroring the byte-count tiers offered by
 * standalone Better Auth secret generators. */
export const SECRET_LENGTH_OPTIONS: {
  bytes: number
  label: string
  description: string
}[] = [
  { bytes: 32, label: 'Standard', description: '32 bytes — recommended for most secrets' },
  { bytes: 48, label: 'Enhanced', description: '48 bytes — higher security for sensitive data' },
  { bytes: 64, label: 'Maximum', description: '64 bytes — maximum security for enterprise use' },
]

/** Returns generation params for `key` if it's a recognized local secret, else null. */
export function getGeneratedSecretSpec(key: string): GeneratedSecretSpec | null {
  const upperKey = key.trim().toUpperCase()
  if (!upperKey) return null

  const known = KNOWN_SECRET_VAR_SPECS[upperKey]
  if (known) return known

  if (!/(_SECRET|_KEY|_PASSWORD)$/.test(upperKey)) return null
  if (SERVICE_CONNECTION_PASSWORD_NAMES.has(upperKey)) return null

  const belongsToExternalProvider = EXTERNAL_PROVIDER_PREFIXES.some(
    (prefix) => upperKey === prefix || upperKey.startsWith(`${prefix}_`)
  )
  if (belongsToExternalProvider) return null

  return { bytes: 32, encoding: 'hex' }
}

function toBase64(bytes: Uint8Array): string {
  let binary = ''
  bytes.forEach((b) => {
    binary += String.fromCharCode(b)
  })
  return btoa(binary)
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

export function generateSecretValue(spec: GeneratedSecretSpec): string {
  const bytes = crypto.getRandomValues(new Uint8Array(spec.bytes))
  const encoded = spec.encoding === 'hex' ? toHex(bytes) : toBase64(bytes)
  return spec.prefix ? `${spec.prefix}${encoded}` : encoded
}

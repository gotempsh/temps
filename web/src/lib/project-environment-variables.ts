// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as z from 'zod/v4'

export const PROJECT_ENVIRONMENT_VARIABLE_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/

const LIKELY_SECRET_KEY =
  /(?:^|_)(?:SECRET|PASSWORD|PASSWD|TOKEN|API_KEY|PRIVATE_KEY|ACCESS_KEY|DATABASE_URL|POSTGRES_URL|MYSQL_URL|MONGODB_URL|MONGODB_URI|REDIS_URL|AMQP_URL|CONNECTION_STRING|DSN|WEBHOOK_URL)(?:_|$)/i

/**
 * Variables written after user configuration is resolved by the deployment
 * planner. Letting users configure these during project creation is misleading:
 * the generated value wins at deploy time. OTEL_EXPORTER_OTLP_TOKEN is kept in
 * this list as a legacy alias used by an older bundled template; Temps now
 * injects the standard OTEL_EXPORTER_OTLP_HEADERS value instead.
 *
 * This is only a conservative client-side validation guard. The inventory
 * shown to users comes from the backend managed-environment-variables endpoint,
 * whose canonical catalog lives in
 * `temps-deployments/src/services/managed_environment_variables.rs`.
 */
export const TEMPS_MANAGED_PROJECT_ENVIRONMENT_VARIABLES = [
  'SENTRY_DSN',
  'SENTRY_TUNNEL',
  'NEXT_PUBLIC_SENTRY_DSN',
  'NUXT_PUBLIC_SENTRY_DSN',
  'VITE_SENTRY_DSN',
  'PUBLIC_SENTRY_DSN',
  'REACT_APP_SENTRY_DSN',
  'NEXT_PUBLIC_SENTRY_TUNNEL',
  'NUXT_PUBLIC_SENTRY_TUNNEL',
  'VITE_SENTRY_TUNNEL',
  'PUBLIC_SENTRY_TUNNEL',
  'REACT_APP_SENTRY_TUNNEL',
  'SENTRY_RELEASE',
  'TEMPS_API_URL',
  'TEMPS_API_TOKEN',
  'CRON_SECRET',
  'PORT',
  'TEMPS_ASSET_PREFIX',
  'NEXT_PUBLIC_TEMPS_ASSET_PREFIX',
  'TEMPS_NODE_NAME',
  'TEMPS_NODE_ID',
  'TEMPS_REPLICA',
  'OTEL_EXPORTER_OTLP_ENDPOINT',
  'OTEL_EXPORTER_OTLP_PROTOCOL',
  'OTEL_EXPORTER_OTLP_HEADERS',
  'OTEL_EXPORTER_OTLP_TOKEN',
  'OTEL_SERVICE_NAME',
  'OTEL_SERVICE_VERSION',
] as const

const TEMPS_MANAGED_PROJECT_ENVIRONMENT_VARIABLE_SET = new Set<string>(
  TEMPS_MANAGED_PROJECT_ENVIRONMENT_VARIABLES
)

export function isTempsManagedProjectEnvironmentVariable(key: string): boolean {
  return TEMPS_MANAGED_PROJECT_ENVIRONMENT_VARIABLE_SET.has(key.trim())
}

/**
 * Picks a safe initial value for the explicit "Encrypt as secret" control.
 * This intentionally avoids broad matches such as `AUTH`, which would mark
 * public values like `NEXTAUTH_URL` as write-only.
 */
export function isLikelySecretProjectEnvironmentVariable(key: string): boolean {
  return LIKELY_SECRET_KEY.test(key.trim())
}

export const projectEnvironmentVariablesSchema = z
  .array(
    z
      .object({
        key: z
          .string()
          .trim()
          .min(1, 'Variable name is required')
          .regex(
            PROJECT_ENVIRONMENT_VARIABLE_KEY,
            'Use letters, numbers, and underscores; the name cannot start with a number'
          ),
        value: z.string(),
        isSecret: z.boolean(),
      })
      // A secret is write-only once saved, so an empty one could never be
      // filled in afterwards — the server rejects it too.
      .refine((variable) => !variable.isSecret || variable.value.length > 0, {
        message: 'A secret needs a value — it cannot be filled in later',
        path: ['value'],
      })
      .refine(
        (variable) => !isTempsManagedProjectEnvironmentVariable(variable.key),
        {
          message: 'Temps provides this variable automatically at deployment',
          path: ['key'],
        }
      )
  )
  .superRefine((variables, context) => {
    const firstIndexByKey = new Map<string, number>()

    variables.forEach((variable, index) => {
      if (
        !variable.key ||
        !PROJECT_ENVIRONMENT_VARIABLE_KEY.test(variable.key)
      ) {
        return
      }

      const firstIndex = firstIndexByKey.get(variable.key)
      if (firstIndex === undefined) {
        firstIndexByKey.set(variable.key, index)
        return
      }

      context.addIssue({
        code: 'custom',
        message: `${variable.key} is already defined`,
        path: [index, 'key'],
      })
    })
  })

export type ProjectEnvironmentVariable = z.infer<
  typeof projectEnvironmentVariablesSchema
>[number]

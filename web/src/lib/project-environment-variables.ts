// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as z from 'zod/v4'

export const PROJECT_ENVIRONMENT_VARIABLE_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/

const LIKELY_SECRET_KEY =
  /(?:^|_)(?:SECRET|PASSWORD|PASSWD|TOKEN|API_KEY|PRIVATE_KEY|ACCESS_KEY|DATABASE_URL|POSTGRES_URL|MYSQL_URL|MONGODB_URL|MONGODB_URI|REDIS_URL|AMQP_URL|CONNECTION_STRING|DSN|WEBHOOK_URL)(?:_|$)/i

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

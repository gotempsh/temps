// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Validation for Settings > Build limits.
 *
 * Every field is a required number. A cleared `type="number"` input reads as
 * `NaN`, which `JSON.stringify` sends as `null`; the server stores these as
 * unsigned integers (and a float for CPU), so a `null` fails the whole
 * settings save. Refusing it here names the field instead.
 */
import { z } from 'zod'

const between = (min: number, max: number) =>
  `Must be between ${min} and ${max}`

export const buildLimitsFormSchema = z.object({
  build_limits: z.object({
    max_concurrent: z
      .number({ error: between(1, 32) })
      .int('Use a whole number')
      .min(1, between(1, 32))
      .max(32, between(1, 32)),
    cpu_limit_cores: z
      .number({ error: 'Enter a value; use 0 for 50% of host CPU.' })
      .min(0, between(0, 64))
      .max(64, between(0, 64)),
    memory_limit_mb: z
      .number({ error: 'Enter a value; use 0 for 50% of host memory.' })
      .int('Use a whole number of MB')
      .min(0, between(0, 262144))
      .max(262144, between(0, 262144)),
    build_cache_max_gb: z
      .number({ error: 'Enter a value; use 0 to disable the size cap.' })
      .int('Use a whole number of GB')
      .min(0, between(0, 10000))
      .max(10000, between(0, 10000)),
    build_cache_max_unused_days: z
      .number({ error: between(1, 365) })
      .int('Use a whole number of days')
      .min(1, between(1, 365))
      .max(365, between(1, 365)),
  }),
})

export type BuildLimitsFormData = z.infer<typeof buildLimitsFormSchema>

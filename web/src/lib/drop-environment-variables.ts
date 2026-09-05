// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface DropEnvironmentVariable {
  id: string
  key: string
  value: string
  /** Adds stricter masking and permission-checked, audited reveals. */
  isSecret: boolean
}

const ENVIRONMENT_VARIABLE_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/

export function validateDropEnvironmentVariables(
  variables: DropEnvironmentVariable[]
): string | null {
  const seen = new Set<string>()

  for (const variable of variables) {
    const key = variable.key.trim()
    if (!key) return 'Every environment variable needs a key'
    if (!ENVIRONMENT_VARIABLE_KEY.test(key)) {
      return `${key} is not a valid environment variable key`
    }
    if (seen.has(key)) return `${key} is defined more than once`
    // Empty secrets are unusable credentials. The server rejects them too.
    if (variable.isSecret && !variable.value) {
      return `${key} is marked as a secret but has no value`
    }
    seen.add(key)
  }

  return null
}

export function serializeDropEnvironmentVariables(
  variables: DropEnvironmentVariable[]
): Array<{ key: string; value: string; is_secret: boolean }> {
  return variables.map((variable) => ({
    key: variable.key.trim(),
    value: variable.value,
    is_secret: variable.isSecret,
  }))
}

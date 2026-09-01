// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface DropEnvironmentVariable {
  id: string
  key: string
  value: string
  /**
   * Write-only: temps stores the value encrypted and never returns the
   * plaintext again, so it can be replaced but never read back.
   */
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
    // A secret is write-only once saved, so an empty one could never be
    // filled in afterwards. The server rejects it too.
    if (variable.isSecret && !variable.value) {
      return `${key} is marked as a secret but has no value — secrets cannot be filled in later`
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

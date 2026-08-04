export interface DropEnvironmentVariable {
  id: string
  key: string
  value: string
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
    seen.add(key)
  }

  return null
}

export function serializeDropEnvironmentVariables(
  variables: DropEnvironmentVariable[]
): Array<[string, string]> {
  return variables.map((variable) => [variable.key.trim(), variable.value])
}

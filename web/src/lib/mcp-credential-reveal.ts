// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const MASKED_CREDENTIAL_VALUE = '***'

type JsonObject = Record<string, unknown>

function parseConfig(configText: string): JsonObject {
  const parsed: unknown = JSON.parse(configText)
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('MCP configuration must be a JSON object')
  }
  return parsed as JsonObject
}

function maskedFieldsInObject(
  config: JsonObject,
  objectName: 'env' | 'headers'
): string[] {
  const values = config[objectName]
  if (!values || typeof values !== 'object' || Array.isArray(values)) {
    return []
  }

  return Object.entries(values)
    .filter(([, value]) => value === MASKED_CREDENTIAL_VALUE)
    .map(([key]) => `${objectName}.${key}`)
}

export function listMaskedMcpCredentialFields(configText: string): string[] {
  let config: JsonObject
  try {
    config = parseConfig(configText)
  } catch {
    return []
  }

  const fields = [
    ...(config.url === MASKED_CREDENTIAL_VALUE ? ['url'] : []),
    ...maskedFieldsInObject(config, 'env'),
    ...maskedFieldsInObject(config, 'headers'),
  ]

  return fields.sort()
}

export function replaceMcpCredentialValue(
  configText: string,
  field: string,
  value: string
): string {
  const config = parseConfig(configText)

  if (field === 'url') {
    config.url = value
    return JSON.stringify(config, null, 2)
  }

  const separator = field.indexOf('.')
  if (separator === -1) {
    throw new Error(`Unsupported MCP credential field: ${field}`)
  }

  const objectName = field.slice(0, separator)
  const key = field.slice(separator + 1)
  if ((objectName !== 'env' && objectName !== 'headers') || !key) {
    throw new Error(`Unsupported MCP credential field: ${field}`)
  }

  const existing = config[objectName]
  if (!existing || typeof existing !== 'object' || Array.isArray(existing)) {
    throw new Error(`MCP configuration has no ${objectName} object`)
  }

  ;(existing as JsonObject)[key] = value
  return JSON.stringify(config, null, 2)
}

export function revealMcpCredentialValueIfStillMasked(
  configText: string,
  field: string,
  revealedValue: string
): string | undefined {
  let config: JsonObject
  try {
    config = parseConfig(configText)
  } catch {
    return undefined
  }

  if (field === 'url') {
    if (config.url !== MASKED_CREDENTIAL_VALUE) return undefined
    config.url = revealedValue
    return JSON.stringify(config, null, 2)
  }

  const separator = field.indexOf('.')
  if (separator === -1) return undefined
  const objectName = field.slice(0, separator)
  const key = field.slice(separator + 1)
  if ((objectName !== 'env' && objectName !== 'headers') || !key) {
    return undefined
  }
  const existing = config[objectName]
  if (!existing || typeof existing !== 'object' || Array.isArray(existing)) {
    return undefined
  }
  const values = existing as JsonObject
  if (values[key] !== MASKED_CREDENTIAL_VALUE) return undefined
  values[key] = revealedValue
  return JSON.stringify(config, null, 2)
}

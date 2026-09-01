// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { load } from 'js-yaml'

export interface ComposePortMapping {
  target: number
  published?: number
  protocol: string
}

export interface ComposeServicePorts {
  name: string
  image?: string
  ports: ComposePortMapping[]
}

type UnknownRecord = Record<string, unknown>

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function validPort(value: unknown): number | undefined {
  const parsed =
    typeof value === 'number'
      ? value
      : typeof value === 'string' && /^\d+$/.test(value.trim())
        ? Number(value)
        : Number.NaN
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 65535
    ? parsed
    : undefined
}

export function parseComposePort(value: unknown): ComposePortMapping | null {
  if (typeof value === 'number') {
    const target = validPort(value)
    return target ? { target, protocol: 'tcp' } : null
  }

  if (typeof value === 'string') {
    const slash = value.lastIndexOf('/')
    const rawPorts = slash >= 0 ? value.slice(0, slash) : value
    const protocol = slash >= 0 ? value.slice(slash + 1) || 'tcp' : 'tcp'
    const segments = rawPorts.split(':')
    const target = validPort(segments[segments.length - 1])
    if (!target) return null
    const published =
      segments.length >= 2
        ? validPort(segments[segments.length - 2])
        : undefined
    return { target, published, protocol }
  }

  if (!isRecord(value)) return null
  const target = validPort(value.target)
  if (!target) return null
  return {
    target,
    published: validPort(value.published),
    protocol:
      typeof value.protocol === 'string' && value.protocol
        ? value.protocol
        : 'tcp',
  }
}

export function parseComposeOverridePorts(
  yaml: string | undefined
): ComposeServicePorts[] {
  if (!yaml?.trim()) return []
  try {
    const document = load(yaml)
    if (!isRecord(document) || !isRecord(document.services)) return []
    return Object.entries(document.services).flatMap(([name, service]) => {
      if (!isRecord(service) || !Array.isArray(service.ports)) return []
      const ports = service.ports
        .map(parseComposePort)
        .filter((port): port is ComposePortMapping => port !== null)
      return ports.length ? [{ name, ports }] : []
    })
  } catch {
    // Saving the override reports invalid YAML. Suggestions retain the last
    // valid snapshot while a draft is incomplete.
    return []
  }
}

export function mergeComposeServicePorts(
  ...sources: ComposeServicePorts[][]
): ComposeServicePorts[] {
  const services = new Map<string, Map<string, ComposePortMapping>>()
  const images = new Map<string, string>()
  for (const source of sources) {
    for (const service of source) {
      if (service.image) images.set(service.name, service.image)
      const ports = services.get(service.name) ?? new Map()
      for (const port of service.ports) {
        const key = `${port.target}/${port.protocol || 'tcp'}`
        const existing = ports.get(key)
        ports.set(key, {
          ...existing,
          ...port,
          protocol: port.protocol || 'tcp',
        })
      }
      services.set(service.name, ports)
    }
  }
  return [...services.entries()].map(([name, ports]) => ({
    name,
    image: images.get(name),
    ports: [...ports.values()].sort((a, b) => a.target - b.target),
  }))
}

export function findComposePortMapping(
  services: ComposeServicePorts[],
  serviceName: string,
  configuredPort: number
): ComposePortMapping | undefined {
  return services
    .find((service) => service.name === serviceName)
    ?.ports.find(
      (port) =>
        port.target === configuredPort || port.published === configuredPort
    )
}

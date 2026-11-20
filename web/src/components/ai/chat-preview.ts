// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Highest TCP port number. */
const MAX_PORT = 65535

/** A port typed by the user, or null when it is not one. */
export function parsePort(text: string): number | null {
  const trimmed = text.trim()
  if (!/^\d{1,5}$/.test(trimmed)) return null
  const port = Number(trimmed)
  return port >= 1 && port <= MAX_PORT ? port : null
}

/** The page for `port`, from the template the server gives (`{port}` is replaced). */
export function previewUrlFor(template: string, port: number): string {
  return template.replace('{port}', String(port))
}

/**
 * Whether the page can be shown inside the console. A browser refuses to load
 * an `http` page into an `https` one, so the user is sent to a new tab then.
 */
export function canEmbed(pageUrl: string, consoleProtocol: string): boolean {
  return !(consoleProtocol === 'https:' && pageUrl.startsWith('http:'))
}

/** Where in the chat's preview the address bar points. */
export interface PreviewTarget {
  port: number
  /** Starts with `/`; may carry a query and a fragment. */
  path: string
}

/** Stands in for `{port}` while the template's origin is taken apart. */
const PORT_SENTINEL = '54321'

/** A path (with optional query and fragment) as the preview server would see it, or null. */
export function normalizePreviewPath(text: string): string | null {
  const trimmed = text.trim()
  const raw =
    trimmed === '' ? '/' : trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f\s]/.test(raw)) return null
  let parsed: URL
  try {
    parsed = new URL(raw, 'http://preview.invalid')
  } catch {
    return null
  }
  // `//other.host` and `/\other.host` are addresses on another host, not paths.
  if (parsed.origin !== 'http://preview.invalid') return null
  return `${parsed.pathname}${parsed.search}${parsed.hash}`
}

/** The page address for `target`: the template's origin for its port, then the path. */
export function previewAddress(
  template: string,
  target: PreviewTarget
): string {
  const base = previewUrlFor(template, target.port)
  try {
    return `${new URL(base).origin}${target.path}`
  } catch {
    return `${base.replace(/\/+$/, '')}${target.path}`
  }
}

/**
 * What the user typed in the address bar, read as a place in this chat's
 * preview, or null when it is not one. Accepted: a path (`/pricing`), a path
 * without its slash (`pricing`), a port (`5173`), a port and path
 * (`:5173/about`), and a full address on the preview's own host. Anything that
 * would point the frame at another host is refused, so the bar cannot be used
 * to load an arbitrary site inside the console.
 */
export function parsePreviewTarget(
  input: string,
  currentPort: number | null,
  template: string
): PreviewTarget | null {
  const text = input.trim()

  const asPort = parsePort(text)
  if (asPort !== null) return { port: asPort, path: '/' }

  const withPort = /^:(\d{1,5})(.*)$/.exec(text)
  if (withPort) {
    const port = parsePort(withPort[1])
    const path = normalizePreviewPath(withPort[2])
    return port !== null && path !== null ? { port, path } : null
  }

  if (/^https?:\/\//i.test(text)) return parseFullAddress(text, template)

  if (currentPort === null) return null
  const path = normalizePreviewPath(text)
  return path === null ? null : { port: currentPort, path }
}

function parseFullAddress(
  text: string,
  template: string
): PreviewTarget | null {
  let address: URL
  let origin: string
  try {
    address = new URL(text)
    origin = new URL(template.replace('{port}', PORT_SENTINEL)).origin
  } catch {
    return null
  }
  const escaped = origin
    .replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    .split(PORT_SENTINEL)
    .join('(\\d{1,5})')
  const match = new RegExp(`^${escaped}$`).exec(address.origin)
  if (!match) return null
  const port = parsePort(match[1] ?? '')
  const path = normalizePreviewPath(
    `${address.pathname}${address.search}${address.hash}`
  )
  return port !== null && path !== null ? { port, path } : null
}

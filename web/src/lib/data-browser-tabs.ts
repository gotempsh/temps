// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { BrowserTab } from '@/components/storage/DataBrowserTabs'

/**
 * Tabs are persisted to the URL (?tabs=<base64>) so that a page reload or a
 * shared link restores the full workspace. Base64 keeps the URL readable-ish
 * while avoiding URI encoding blowup on nested JSON.
 */

function toBase64(input: string): string {
  const bytes = new TextEncoder().encode(input)
  let binary = ''
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i])
  }
  return window.btoa(binary)
}

function fromBase64(input: string): string {
  const binary = window.atob(input)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i)
  }
  return new TextDecoder().decode(bytes)
}

export function encodeTabs(tabs: BrowserTab[]): string {
  if (typeof window === 'undefined') return ''
  try {
    return toBase64(JSON.stringify(tabs))
  } catch {
    return ''
  }
}

export function decodeTabs(raw: string | null): BrowserTab[] {
  if (!raw || typeof window === 'undefined') return []
  try {
    const parsed = JSON.parse(fromBase64(raw))
    if (!Array.isArray(parsed)) return []
    return parsed.filter(
      (t): t is BrowserTab =>
        t && typeof t === 'object' && typeof t.id === 'string'
    )
  } catch {
    return []
  }
}

export function makeTabId(): string {
  return `t_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`
}

export function newEmptyTab(): BrowserTab {
  return { id: makeTabId(), path: '' }
}

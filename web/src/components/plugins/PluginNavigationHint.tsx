// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { NavEntry } from '@/types/plugins'

export function PluginNavigationHint({ nav }: { nav: NavEntry[] }) {
  if (nav.some((entry) => entry.section === 'platform')) return null
  const message = nav.some((entry) => entry.section === 'settings')
    ? 'Open this plugin from the Settings sidebar.'
    : nav.some((entry) => entry.section === 'project')
      ? 'Open a project to find this plugin in its sidebar.'
      : 'This plugin declares no sidebar page. It can run in the background; its author must add a navigation entry to expose a page.'
  return <p className="mt-1 text-sm text-muted-foreground">{message}</p>
}

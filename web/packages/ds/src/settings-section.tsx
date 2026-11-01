// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useContext, useId, type ReactNode } from 'react'
import { SettingsSurfaceContext } from './settings-surface-context'
import type { LucideIcon } from 'lucide-react'
import { cn } from './lib/cn'

/**
 * Marks its children as already inside a bordered settings surface, so any
 * `SettingsSection` within renders as a flat sub-section instead of a second
 * card. Use it around section content placed inside a page's own card.
 */
export function SettingsSurface({
  children,
  active = true,
}: {
  children: ReactNode
  /**
   * `false` starts a fresh surface below this point, e.g. inside a dialog
   * opened from a settings page, where cards keep their own frame.
   */
  active?: boolean
}) {
  return (
    <SettingsSurfaceContext.Provider value={active}>
      {children}
    </SettingsSurfaceContext.Provider>
  )
}

/**
 * A group of related settings, always open: settings are read and edited in
 * place, never behind a click to expand.
 *
 * At the top level it is an open group: title and description on the left,
 * settings on the right, a rule between groups. Inside another section (or a
 * `SettingsSurface`) it renders flat, as a sub-heading with a divider. Never
 * a card, and never a box inside a box.
 */
export function SettingsSection({
  id,
  title,
  icon: Icon,
  children,
  description,
  className,
}: {
  /** DOM id so a page can deep-link to the section (`?section=<id>`). */
  id?: string
  title: string
  icon: LucideIcon
  children: ReactNode
  /** @deprecated Sections are always open; kept so callers still compile. */
  defaultOpen?: boolean
  /** @deprecated Sections are always open, so errors are always visible. */
  hasError?: boolean
  description?: string
  className?: string
}) {
  const nested = useContext(SettingsSurfaceContext)
  const headingId = `${useId()}-heading`
  const heading = (
    <div className="flex items-start gap-2">
      <Icon
        className="mt-0.5 size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
      <div className="min-w-0">
        <h3 id={headingId} className="text-sm font-semibold">
          {title}
        </h3>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>
    </div>
  )

  if (nested) {
    return (
      <section
        id={id}
        aria-labelledby={headingId}
        className={cn(
          'min-w-0 scroll-mt-4 space-y-4 border-t pt-5 first:border-t-0 first:pt-0',
          className
        )}
      >
        {heading}
        <div className="min-w-0">{children}</div>
      </section>
    )
  }

  // Top level: an open group like `SettingsGroup`, title and purpose on the
  // left, settings on the right, separated from the next group by a rule.
  return (
    <section
      id={id}
      aria-labelledby={headingId}
      className={cn(
        // No rule above the first group, nor right under the page title.
        'grid min-w-0 scroll-mt-4 gap-5 border-t pt-8 first:border-t-0 first:pt-0 [[data-page-header]+&]:border-t-0 [[data-page-header]+&]:pt-2 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10',
        className
      )}
    >
      <div className="min-w-0">
        <h2 id={headingId} className="text-base font-semibold">
          {title}
        </h2>
        {description && (
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      <div className="min-w-0 space-y-5">
        <SettingsSurface>{children}</SettingsSurface>
      </div>
    </section>
  )
}

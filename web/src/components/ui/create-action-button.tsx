// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Link } from 'react-router'
import { Plus } from 'lucide-react'
import { Button, type ButtonProps } from '@/components/ui/button'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'

type CommonProps = {
  /** Label shown inside the button (e.g. "Add Domain", "New Skill"). */
  label: React.ReactNode
  /** Icon rendered on the left of the label. Defaults to a Plus icon. */
  icon?: React.ReactNode
  /** Keyboard shortcut key to register and display. Defaults to "N". */
  shortcutKey?: string
  /** Hide the keyboard badge while still registering the shortcut. */
  hideShortcut?: boolean
  /** Disable both the click handler and the keyboard shortcut. */
  disabled?: boolean
  /** Forward additional classes to the underlying Button. */
  className?: string
} & Pick<ButtonProps, 'variant' | 'size'>

type AsButtonProps = CommonProps & {
  onClick: () => void
  to?: never
}

type AsLinkProps = CommonProps & {
  to: string
  onClick?: never
}

export type CreateActionButtonProps = AsButtonProps | AsLinkProps

/**
 * Primary "Add X" / "New X" action button. Registers a keyboard shortcut
 * (default `N`) and renders a matching `KbdBadge` so the shortcut is
 * discoverable. Use `to` for navigation to a create page, or `onClick` to
 * open a dialog.
 *
 * @example
 * // Navigates to /projects/new on click or when `N` is pressed.
 * <CreateActionButton to="/projects/new" label="New Project" />
 *
 * @example
 * // Opens a dialog.
 * <CreateActionButton onClick={() => setOpen(true)} label="Add MCP Server" />
 */
export function CreateActionButton({
  label,
  icon,
  shortcutKey = 'N',
  hideShortcut = false,
  disabled = false,
  className,
  variant,
  size,
  ...rest
}: CreateActionButtonProps) {
  const onClick = 'onClick' in rest ? rest.onClick : undefined
  const to = 'to' in rest ? rest.to : undefined

  useKeyboardShortcut({
    key: shortcutKey,
    path: to,
    callback: onClick,
    enabled: !disabled,
  })

  const resolvedIcon = icon ?? <Plus className="h-4 w-4" />

  const inner = (
    <>
      {resolvedIcon}
      <span>{label}</span>
      {!hideShortcut && <KbdBadge keys={shortcutKey} />}
    </>
  )

  if (to) {
    return (
      <Button
        asChild
        variant={variant}
        size={size}
        className={className}
        disabled={disabled}
      >
        <Link to={to}>{inner}</Link>
      </Button>
    )
  }

  return (
    <Button
      onClick={onClick}
      variant={variant}
      size={size}
      className={className}
      disabled={disabled}
    >
      {inner}
    </Button>
  )
}

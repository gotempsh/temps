// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Elements inside a row that keep their own click: native controls, custom
 * widgets by their ARIA role, editable text, and anything marked
 * `data-row-ignore` (a value meant to be selected or copied, for one).
 */
const ROW_CONTROLS = [
  'a',
  'button',
  'input',
  'select',
  'textarea',
  'label',
  'summary',
  '[contenteditable]:not([contenteditable=false])',
  ...[
    'button',
    'checkbox',
    'combobox',
    'link',
    'menuitem',
    'menuitemcheckbox',
    'menuitemradio',
    'option',
    'radio',
    'searchbox',
    'slider',
    'spinbutton',
    'switch',
    'tab',
    'textbox',
    'treeitem',
  ].map((role) => `[role=${role}]`),
  '[data-row-ignore]',
].join(',')

/**
 * Whether a click on `target` should follow the row's record link: not when
 * it lands on a control of its own (the link itself included, which already
 * navigates), and not from a portal (a menu or dialog opened from the row),
 * whose events bubble through React to the row.
 */
export function followsRecordLink(row: Element, target: Element): boolean {
  if (!row.contains(target)) return false
  const control = target.closest(ROW_CONTROLS)
  return !control || control === row || !row.contains(control)
}

/** Disabled controls; they often take no pointer events at all. */
const DISABLED_CONTROLS =
  'button:disabled,input:disabled,select:disabled,textarea:disabled,[aria-disabled=true]'

/**
 * Whether the point (`x`, `y`, viewport pixels) lies on a disabled control
 * inside `row`. Disabled buttons use `pointer-events: none`, so a click on
 * one reaches the row with the cell behind it as its target; this tells it
 * apart from a click on empty space. Controls with no box are never hit.
 */
export function onDisabledControl(row: Element, x: number, y: number): boolean {
  for (const control of row.querySelectorAll(DISABLED_CONTROLS)) {
    const box = control.getBoundingClientRect()
    if (box.width === 0 || box.height === 0) continue
    if (x >= box.left && x <= box.right && y >= box.top && y <= box.bottom) {
      return true
    }
  }
  return false
}

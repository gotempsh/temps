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

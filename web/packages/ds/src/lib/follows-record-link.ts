// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Elements inside a row that keep their own click. */
const ROW_CONTROLS =
  'a,button,input,select,textarea,label,summary,[role=button],[role=checkbox],[role=menuitem],[role=switch],[data-row-ignore]'

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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export async function writeToClipboard(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value)
    return
  }

  const textarea = document.createElement('textarea')
  textarea.value = value
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.top = '0'
  textarea.style.left = '0'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)

  const previous = document.activeElement as HTMLElement | null
  try {
    textarea.select()
    textarea.setSelectionRange(0, value.length)
    if (!document.execCommand('copy')) {
      throw new Error(
        'The browser refused the copy. This page is not on a secure origin (https or localhost), so copying has to be done manually.'
      )
    }
  } finally {
    document.body.removeChild(textarea)
    previous?.focus?.()
  }
}

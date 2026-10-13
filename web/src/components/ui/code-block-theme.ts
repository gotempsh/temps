// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// github-dark paints comments #6a737d, which is 3.45:1 on the console's dark
// code panel (WCAG 1.4.3 needs 4.5:1). Use github-dark-default's comment grey,
// which is 5.3:1 there, and leave every other token colour untouched.
const LOW_CONTRAST_COMMENT = '#6a737d'
const READABLE_DARK_COMMENT = '#8b949e'

interface ThemeWithTokenColors {
  tokenColors?: Array<{ settings?: { foreground?: string } }>
}

export function withReadableDarkComments<T extends ThemeWithTokenColors>(
  theme: T
): T {
  return {
    ...theme,
    tokenColors: theme.tokenColors?.map((rule) =>
      rule.settings?.foreground?.toLowerCase() === LOW_CONTRAST_COMMENT
        ? {
            ...rule,
            settings: { ...rule.settings, foreground: READABLE_DARK_COMMENT },
          }
        : rule
    ),
  }
}

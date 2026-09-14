// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Static, trusted markup only: never interpolate preview URLs, grants or errors.
// Paint synchronously before requesting a signed link, keeping the new tab
// inside the user gesture required by browser popup blockers.
export const PREVIEW_LOADING_PAGE = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Opening preview… · Temps</title>
<style>
  :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
  body { margin: 0; background: Canvas; color: CanvasText; }
  main { min-height: 100dvh; display: grid; place-content: center; padding: 24px; box-sizing: border-box; }
  h1 { font-size: 24px; font-weight: 600; margin: 20px 0 8px; }
  p { font-size: 16px; line-height: 1.5; margin: 0; max-width: 36ch; }
  .brand { width: 185px; height: 80px; margin: 0 0 28px -12px; }
  .brand-ink { fill: CanvasText; }
  .brand-paper { fill: Canvas; }
  .spinner { width: 24px; height: 24px; border: 2px solid GrayText; border-top-color: transparent; border-radius: 50%; animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spinner { animation: none; } }
</style></head><body><main role="status" aria-live="polite">
<!-- Self-contained version of public/svg/temps-logo-light.svg; no asset fetch while authorizing. -->
<svg class="brand" role="img" aria-label="Temps" viewBox="0 0 185 80" width="185" height="80">
  <g transform="translate(12, 12) scale(1.75)">
    <rect class="brand-ink" width="32" height="32" rx="6"/>
    <text class="brand-paper" x="16" y="26" font-family="Inter, Segoe UI, Arial, sans-serif" font-weight="900" font-size="28" text-anchor="middle">t</text>
  </g>
  <text class="brand-ink" x="80" y="51" font-family="Inter, Segoe UI, Arial, sans-serif" font-weight="900" font-size="30">temps</text>
</svg>
<div class="spinner" aria-hidden="true"></div>
<h1>Opening preview…</h1>
<p>Preparing secure access to your workspace. This tab will open the preview automatically.</p>
</main></body></html>`

export function showPreviewLoadingPage(tab: Window) {
  tab.document.open()
  tab.document.write(PREVIEW_LOADING_PAGE)
  tab.document.close()
}

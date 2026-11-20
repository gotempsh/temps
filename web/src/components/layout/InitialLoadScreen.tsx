// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The very first paint, shown while the console's code is still downloading.
 * It can't depend on anything the app loads (no auth, no query client), so it
 * is plain markup plus two CSS animations from `globals.css`: the mark pulses
 * and a segment slides along the track, so it reads as "working" rather than
 * a frozen page. Reduced-motion users get a slow fade instead of nothing,
 * because a static bar looks identical to a hung load.
 */
export function InitialLoadScreen() {
  return (
    <div
      role="status"
      aria-label="Loading application"
      className="flex min-h-dvh items-center justify-center bg-background text-foreground"
    >
      <div className="flex flex-col items-center gap-6">
        <div className="initial-load-mark flex items-center gap-2">
          <img src="/favicon.png" alt="" className="size-8" />
          <span className="text-xl font-semibold tracking-tight">Temps</span>
        </div>
        <div className="h-1 w-32 overflow-hidden rounded-full bg-muted">
          <div className="initial-load-bar h-full w-1/3 rounded-full bg-foreground/70" />
        </div>
      </div>
    </div>
  )
}

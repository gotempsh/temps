// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What the local screenshot service does on this server, shown beside the
 * provider choice: it runs a browser here, limits what a page can reach, and
 * may run without Chromium's own sandbox.
 */
export function LocalScreenshotNote() {
  return (
    <p id="screenshot-local-note" className="text-sm text-muted-foreground">
      The local service opens each deployment in a headless Chromium on this
      server. A page can only reach its own address and public internet
      addresses: requests to private, loopback and link-local addresses are
      blocked. Chromium&apos;s own sandbox is used when the host allows it;
      where it cannot start (for example as root in a container) it runs without
      the sandbox and the server log says so. Use the external API if you would
      rather not run a browser here.
    </p>
  )
}

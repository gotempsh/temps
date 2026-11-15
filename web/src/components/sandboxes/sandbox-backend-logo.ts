// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface SandboxBackendLogoSource {
  label: string
  src: string
  /** The file is a logo lockup; show only the mark at its left edge. */
  markOnly?: boolean
}

const LOGOS: Record<string, SandboxBackendLogoSource> = {
  docker: { label: 'Docker', src: '/presets/docker.svg' },
  firecracker: {
    label: 'Firecracker',
    src: '/sandbox-backends/firecracker.svg',
  },
  microsandbox: {
    label: 'microsandbox',
    src: '/sandbox-backends/microsandbox.svg',
    markOnly: true,
  },
}

/** The logo for a sandbox backend, or null when it is unknown or unrecorded. */
export function sandboxBackendLogo(
  backend: string | null | undefined
): SandboxBackendLogoSource | null {
  if (!backend) return null
  return LOGOS[backend.toLowerCase()] ?? null
}

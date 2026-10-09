// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Box, Cpu } from 'lucide-react'

const BACKEND_PRESENTATION: Record<
  string,
  { label: string; title: string; microvm: boolean }
> = {
  docker: { label: 'Docker', title: 'Namespaced container', microvm: false },
  firecracker: {
    label: 'Firecracker',
    title: 'Hardware-virtualized microVM (KVM)',
    microvm: true,
  },
  microsandbox: {
    label: 'microsandbox',
    title: 'libkrun microVM (experimental)',
    microvm: true,
  },
}

/** The isolation backend a sandbox runs on, with a hint of what it means. */
export function SandboxBackendLabel({ backend }: { backend: string }) {
  const presentation = BACKEND_PRESENTATION[backend]
  return (
    <span
      className="inline-flex items-center gap-1"
      title={presentation?.title ?? backend}
    >
      {presentation?.microvm ? (
        <Cpu className="h-3 w-3" />
      ) : (
        <Box className="h-3 w-3" />
      )}
      {presentation?.label ?? backend}
    </span>
  )
}

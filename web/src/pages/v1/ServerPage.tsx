// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `/monitoring/server` inside the legacy console: the `@temps-sdk/ds` page
 * under its own skin root. The operator skin is a class on an element, not a
 * global stylesheet, so the rest of the console keeps its look; when the
 * whole console moves to the `/v1` shell this file goes away and `ServerV1`
 * mounts there unchanged.
 */

import '@temps-sdk/ds/op.css'
import { ServerV1 } from './ServerV1'

export function ServerPage() {
  return (
    <div className="operator ink v1 min-w-0 bg-background text-foreground">
      <ServerV1 />
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { BulkActivationJobResponse } from '@/api/client'
import { ACTIVATION_SECTION_ANCHOR } from '@/components/observe/CloudTelemetryActivationSection'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { ActivationJumpLink } from './OtelPipelineStatusPage'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const JOB = {
  status: 'running',
  projects_done: 1,
  projects_total: 3,
} as BulkActivationJobResponse

test('"Jump to the activation" scrolls the page to it and leaves the shell in place', () => {
  // The activation is the last section of the page, below four charts.
  layout.place(`#${ACTIVATION_SECTION_ANCHOR}`, NEAR_THE_END)
  const view = render(
    <ConsoleShell>
      <ActivationJumpLink job={JOB} />
      <section id={ACTIVATION_SECTION_ANCHOR}>Activation</section>
    </ConsoleShell>
  )
  fireEvent.click(view.getByRole('link', { name: /Jump to the activation/ }))
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})

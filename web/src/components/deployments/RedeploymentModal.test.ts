// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

test('redeploy mode submits the reference that pins the deployment commit', async () => {
  const source = await Bun.file(
    new URL('./RedeploymentModal.tsx', import.meta.url).pathname
  ).text()
  const redeployBranch = source.slice(
    source.indexOf("if (mode === 'redeploy')"),
    source.indexOf('// In new mode')
  )
  // Rebuilding from the branch alone deploys whatever the branch points at
  // now, not what the clicked deployment ran. The commit-pinning rule lives in
  // `redeployGitReference`; the modal must not reintroduce its own.
  expect(redeployBranch).toContain('redeployGitReference(')
  expect(redeployBranch).not.toMatch(
    /defaultType === 'branch' \? defaultBranch/
  )
})

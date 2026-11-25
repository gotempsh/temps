// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useNavigate } from 'react-router'
import { shouldWelcome } from './onboarding'
import { useOnboarding } from './useOnboarding'

/** Where the first-run flow lives. */
export const WELCOME_PATH = '/welcome'

/**
 * Send an instance admin on a fresh console (no projects, setup never
 * finished or skipped) to the welcome flow. `projectCount` is `undefined`
 * until it is known, so nothing happens on a partial answer.
 */
export function useWelcomeRedirect(projectCount: number | undefined) {
  const navigate = useNavigate()
  const onboarding = useOnboarding({ enabled: projectCount === 0 })
  const go = shouldWelcome(onboarding.data, projectCount)
  useEffect(() => {
    if (go) navigate(WELCOME_PATH, { replace: true })
  }, [go, navigate])
}

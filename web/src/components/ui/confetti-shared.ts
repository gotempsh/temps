// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'

export interface ConfettiProps {
  active: boolean
  duration?: number
  particleCount?: number
}

// Hook to trigger confetti
export function useConfetti() {
  const [showConfetti, setShowConfetti] = useState(false)

  const triggerConfetti = () => {
    setShowConfetti(true)
    setTimeout(() => setShowConfetti(false), 100) // Reset quickly to allow re-triggering
  }

  return { showConfetti, triggerConfetti }
}

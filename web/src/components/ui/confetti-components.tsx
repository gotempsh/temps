// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { type ConfettiProps } from './confetti-shared'

export function Confetti({
  active,
  duration = 3000,
  particleCount = 50,
}: ConfettiProps) {
  return active ? (
    <ActiveConfetti duration={duration} particleCount={particleCount} />
  ) : null
}

function ActiveConfetti({
  duration,
  particleCount,
}: Required<Pick<ConfettiProps, 'duration' | 'particleCount'>>) {
  const [particles, setParticles] = useState<
    Array<{
      id: number
      color: string
      delay: number
      left: number
      rotation: number
    }>
  >(() =>
    Array.from({ length: particleCount }, (_, i) => ({
      id: i,
      color: ['#FFD700', '#FF69B4', '#00CED1', '#FF6347', '#9370DB', '#32CD32'][
        Math.floor(Math.random() * 6)
      ],
      delay: Math.random() * 0.5,
      left: Math.random() * 100,
      rotation: Math.random() * 360,
    }))
  )
  const [isVisible, setIsVisible] = useState(true)

  useEffect(() => {
    const timer = setTimeout(() => {
      setIsVisible(false)
      setParticles([])
    }, duration)
    return () => clearTimeout(timer)
  }, [duration])

  if (!isVisible) return null

  return (
    <div className="pointer-events-none fixed inset-0 z-50 overflow-hidden">
      {particles.map((particle) => (
        <div
          key={particle.id}
          className="absolute h-3 w-3 animate-confetti-fall rounded-sm"
          style={{
            left: `${particle.left}%`,
            animationDelay: `${particle.delay}s`,
            backgroundColor: particle.color,
            transform: `rotate(${particle.rotation}deg)`,
          }}
        />
      ))}
    </div>
  )
}

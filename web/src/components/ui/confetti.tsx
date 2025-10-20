import { useEffect, useState } from 'react'

interface ConfettiProps {
  active: boolean
  duration?: number
  particleCount?: number
}

export function Confetti({
  active,
  duration = 3000,
  particleCount = 50,
}: ConfettiProps) {
  const [particles, setParticles] = useState<
    Array<{
      id: number
      color: string
      delay: number
      left: number
      rotation: number
    }>
  >([])
  const [isVisible, setIsVisible] = useState(false)

  useEffect(() => {
    if (active) {
      setIsVisible(true)
      // Generate random particles
      const newParticles = Array.from({ length: particleCount }, (_, i) => ({
        id: i,
        color: [
          '#FFD700',
          '#FF69B4',
          '#00CED1',
          '#FF6347',
          '#9370DB',
          '#32CD32',
        ][Math.floor(Math.random() * 6)],
        delay: Math.random() * 0.5,
        left: Math.random() * 100,
        rotation: Math.random() * 360,
      }))
      setParticles(newParticles)

      // Hide after duration
      const timer = setTimeout(() => {
        setIsVisible(false)
        setParticles([])
      }, duration)

      return () => clearTimeout(timer)
    }
  }, [active, duration, particleCount])

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

// Hook to trigger confetti
export function useConfetti() {
  const [showConfetti, setShowConfetti] = useState(false)

  const triggerConfetti = () => {
    setShowConfetti(true)
    setTimeout(() => setShowConfetti(false), 100) // Reset quickly to allow re-triggering
  }

  return { showConfetti, triggerConfetti }
}

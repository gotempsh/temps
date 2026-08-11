import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/**
 * Runs `action` but doesn't resolve until at least `minMs` has elapsed, even
 * if `action` finishes sooner. Use this to back a spinner (e.g. Tailwind's
 * `animate-spin`, a 1s/rotation animation) so a fast response doesn't cut the
 * icon off mid-turn — the spin needs to be visible for it to read as feedback.
 */
export async function withMinDuration<T>(
  action: () => Promise<T>,
  minMs = 1000
): Promise<T> {
  const start = Date.now()
  try {
    return await action()
  } finally {
    const elapsed = Date.now() - start
    if (elapsed < minMs) {
      await new Promise((resolve) => setTimeout(resolve, minMs - elapsed))
    }
  }
}

export function formatBytes(bytes: number | null | undefined, decimals = 2) {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return '0 Bytes'

  const k = 1024
  const dm = decimals < 0 ? 0 : decimals
  const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB']

  const i = Math.min(
    sizes.length - 1,
    Math.floor(Math.log(bytes) / Math.log(k)),
  )

  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

export const PASSWORD_REQUIREMENTS = [
  {
    id: 'length',
    label: '8–128 characters',
    test: (password: string) => password.length >= 8 && password.length <= 128,
  },
  {
    id: 'uppercase',
    label: 'One uppercase letter',
    test: (password: string) => /[A-Z]/.test(password),
  },
  {
    id: 'lowercase',
    label: 'One lowercase letter',
    test: (password: string) => /[a-z]/.test(password),
  },
  {
    id: 'number',
    label: 'One number',
    test: (password: string) => /[0-9]/.test(password),
  },
  {
    id: 'special',
    label: 'One special character',
    test: (password: string) => /[^a-zA-Z0-9]/.test(password),
  },
] as const

export const passwordSchema = z
  .string()
  .min(8, 'Password must be at least 8 characters long')
  .max(128, 'Password must not exceed 128 characters')
  .regex(/[A-Z]/, 'Password must contain at least one uppercase letter')
  .regex(/[a-z]/, 'Password must contain at least one lowercase letter')
  .regex(/[0-9]/, 'Password must contain at least one digit')
  .regex(/[^a-zA-Z0-9]/, 'Password must contain at least one special character')

export function passwordRequirementResults(password: string) {
  return PASSWORD_REQUIREMENTS.map((requirement) => ({
    ...requirement,
    met: requirement.test(password),
  }))
}

const PASSWORD_ALPHABETS = {
  uppercase: 'ABCDEFGHJKLMNPQRSTUVWXYZ',
  lowercase: 'abcdefghijkmnopqrstuvwxyz',
  number: '23456789',
  special: '!@#$%^&*_-+=',
} as const

const PASSWORD_CHARACTERS = Object.values(PASSWORD_ALPHABETS).join('')

function secureRandomIndex(length: number): number {
  const maximum = Math.floor(256 / length) * length
  const randomByte = new Uint8Array(1)

  do {
    crypto.getRandomValues(randomByte)
  } while (randomByte[0] >= maximum)

  return randomByte[0] % length
}

/** Generate a copy-friendly password with every server-required character class. */
export function generateCompliantPassword(length = 20): string {
  if (length < 8 || length > 128) {
    throw new RangeError('Password length must be between 8 and 128 characters')
  }

  const characters = Object.values(PASSWORD_ALPHABETS).map(
    (alphabet) => alphabet[secureRandomIndex(alphabet.length)]
  )
  while (characters.length < length) {
    characters.push(
      PASSWORD_CHARACTERS[secureRandomIndex(PASSWORD_CHARACTERS.length)]
    )
  }

  for (let index = characters.length - 1; index > 0; index -= 1) {
    const swapIndex = secureRandomIndex(index + 1)
    ;[characters[index], characters[swapIndex]] = [
      characters[swapIndex],
      characters[index],
    ]
  }

  return characters.join('')
}

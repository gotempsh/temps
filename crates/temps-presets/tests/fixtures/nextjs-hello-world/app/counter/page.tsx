// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { useState } from 'react'

export default function CounterPage() {
  const [count, setCount] = useState(0)

  return (
    <main style={{ padding: '2rem' }}>
      <h1>Counter</h1>
      <p style={{ fontSize: '2rem', margin: '1rem 0' }}>{count}</p>
      <div style={{ display: 'flex', gap: '0.5rem' }}>
        <button onClick={() => setCount(c => c - 1)}>-</button>
        <button onClick={() => setCount(0)}>Reset</button>
        <button onClick={() => setCount(c => c + 1)}>+</button>
      </div>
    </main>
  )
}

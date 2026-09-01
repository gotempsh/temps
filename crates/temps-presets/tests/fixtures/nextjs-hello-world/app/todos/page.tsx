// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { useState } from 'react'

export default function TodosPage() {
  const [todos, setTodos] = useState<{ id: number; text: string; done: boolean }[]>([])
  const [input, setInput] = useState('')

  function addTodo() {
    const text = input.trim()
    if (!text) return
    setTodos(prev => [...prev, { id: Date.now(), text, done: false }])
    setInput('')
  }

  function toggleTodo(id: number) {
    setTodos(prev => prev.map(t => t.id === id ? { ...t, done: !t.done } : t))
  }

  function removeTodo(id: number) {
    setTodos(prev => prev.filter(t => t.id !== id))
  }

  return (
    <main style={{ padding: '2rem' }}>
      <h1>Todos</h1>
      <div style={{ display: 'flex', gap: '0.5rem', margin: '1rem 0' }}>
        <input
          value={input}
          onChange={e => setInput(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && addTodo()}
          placeholder="Add a todo..."
          style={{ padding: '0.25rem 0.5rem', flex: 1 }}
        />
        <button onClick={addTodo}>Add</button>
      </div>
      {todos.length === 0 && <p style={{ color: '#888' }}>No todos yet.</p>}
      <ul style={{ listStyle: 'none', padding: 0 }}>
        {todos.map(todo => (
          <li key={todo.id} style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', padding: '0.25rem 0' }}>
            <input type="checkbox" checked={todo.done} onChange={() => toggleTodo(todo.id)} />
            <span style={{ textDecoration: todo.done ? 'line-through' : 'none', flex: 1 }}>{todo.text}</span>
            <button onClick={() => removeTodo(todo.id)}>x</button>
          </li>
        ))}
      </ul>
    </main>
  )
}

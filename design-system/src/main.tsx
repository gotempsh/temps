// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { ThemeProvider } from 'next-themes'
import { BrowserRouter } from 'react-router'
import { Toaster } from 'sonner'
import App from './App'
import { TooltipProvider } from './components/ui/tooltip'
import './globals.css'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider attribute="class" defaultTheme="light" enableSystem={false}>
      <TooltipProvider>
        <BrowserRouter>
          <App />
          <Toaster expand />
        </BrowserRouter>
      </TooltipProvider>
    </ThemeProvider>
  </StrictMode>
)

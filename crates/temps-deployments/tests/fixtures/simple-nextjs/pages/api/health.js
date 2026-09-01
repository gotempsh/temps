// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Health check API endpoint for deployment verification
 */
export default function handler(req, res) {
  res.status(200).json({
    status: 'healthy',
    framework: 'Next.js',
    version: '14.1.0',
    deployed_with: 'nixpacks',
    timestamp: new Date().toISOString()
  })
}

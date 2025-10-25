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

/**
 * Home page component for testing Nixpacks + Next.js deployment
 */
export default function Home() {
  return (
    <div style={{
      fontFamily: 'system-ui, sans-serif',
      maxWidth: '800px',
      margin: '0 auto',
      padding: '2rem'
    }}>
      <h1 style={{ color: '#0070f3' }}>
        Hello from Nixpacks + Next.js!
      </h1>
      <p>
        This is a simple Next.js application deployed using Nixpacks auto-detection.
      </p>
      <div style={{
        marginTop: '2rem',
        padding: '1rem',
        backgroundColor: '#f5f5f5',
        borderRadius: '8px'
      }}>
        <h2>Deployment Info</h2>
        <ul>
          <li><strong>Framework:</strong> Next.js 14</li>
          <li><strong>Deployed with:</strong> Nixpacks</li>
          <li><strong>Status:</strong> ✅ Running</li>
        </ul>
      </div>
      <div style={{ marginTop: '1rem' }}>
        <a href="/api/health" style={{ color: '#0070f3' }}>
          Check Health API →
        </a>
      </div>
    </div>
  )
}

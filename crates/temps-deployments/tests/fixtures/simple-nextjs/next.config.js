/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  output: 'standalone',
  // Optimize for production deployment
  compress: true,
  poweredByHeader: false,
}

module.exports = nextConfig

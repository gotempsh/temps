export default {
  client: '@hey-api/client-fetch',
  // input: 'https://app.localup.dev/api-docs/openapi.json',
  input: 'http://localhost:8081/api-docs/openapi.json',
  output: 'src/api/client',
  plugins: ['@tanstack/react-query'],
}

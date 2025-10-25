export default {
  client: '@hey-api/client-fetch',
  // input: 'https://app.localup.dev/api-docs/openapi.json',
  input: 'http://localhost:8080/api-docs/openapi.json',
  output: 'src/api/client',
  plugins: ['@tanstack/react-query'],
}

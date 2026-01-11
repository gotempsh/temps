# Go Gin Basic Example

Simple Go Gin API server for testing Go deployments.

## Running Locally

```bash
# Initialize Go modules (if needed)
go mod download

# Run server
go run main.go

# Or build and run
go build -o server
./server
```

## Endpoints

- `GET /` - Hello message with JSON response
- `GET /health` - Health check endpoint

## Environment Variables

- `PORT` - Server port (default: 8080)

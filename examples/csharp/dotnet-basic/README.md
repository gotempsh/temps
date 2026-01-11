# .NET Core Basic Example

A simple .NET 8 minimal API application demonstrating deployment on Temps.

## Features

- Minimal API with .NET 8 (LTS)
- Health check endpoints
- Containerized with Docker
- C# 12

## Endpoints

- `GET /` - Hello endpoint returning JSON response
- `GET /health` - Health check endpoint

## Local Development

### Prerequisites

- .NET 8 SDK or higher

### Running Locally

```bash
dotnet run
```

The application will start on `http://localhost:8080`

### Building

```bash
dotnet build
dotnet run --no-build
```

## Docker

### Build

```bash
docker build -t dotnet-basic .
```

### Run

```bash
docker run -p 8080:8080 dotnet-basic
```

## Environment Variables

- `PORT` - Server port (default: 8080)

## Response Example

```bash
curl http://localhost:8080/
```

```json
{
  "message": "Hello from .NET!",
  "status": "healthy",
  "version": "1.0.0"
}
```

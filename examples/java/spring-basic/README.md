# Spring Boot Basic Example

A simple Spring Boot REST API application demonstrating deployment on Temps.

## Features

- RESTful API with Spring Boot 3.2
- Health check endpoints
- Containerized with Docker
- Java 17

## Endpoints

- `GET /` - Hello endpoint returning JSON response
- `GET /health` - Health check endpoint
- `GET /actuator/health` - Spring Boot Actuator health endpoint

## Local Development

### Prerequisites

- Java 17 or higher
- Maven 3.9+

### Running Locally

```bash
mvn spring-boot:run
```

The application will start on `http://localhost:8080`

### Building with Maven

```bash
mvn clean package
java -jar target/spring-basic-1.0.0.jar
```

## Docker

### Build

```bash
docker build -t spring-basic .
```

### Run

```bash
docker run -p 8080:8080 spring-basic
```

## Environment Variables

- `PORT` - Server port (default: 8080)

## Response Example

```bash
curl http://localhost:8080/
```

```json
{
  "message": "Hello from Spring Boot!",
  "status": "healthy",
  "version": "1.0.0"
}
```

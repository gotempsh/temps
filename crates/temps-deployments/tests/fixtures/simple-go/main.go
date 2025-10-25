// Simple Gin web server for testing Nixpacks deployment
package main

import (
	"fmt"
	"net/http"
	"os"
	"runtime"

	"github.com/gin-gonic/gin"
)

// HealthResponse represents the health check response
type HealthResponse struct {
	Status       string `json:"status"`
	Framework    string `json:"framework"`
	Version      string `json:"version"`
	DeployedWith string `json:"deployed_with"`
	GoVersion    string `json:"go_version"`
}

// InfoResponse represents application info
type InfoResponse struct {
	Name        string `json:"name"`
	Version     string `json:"version"`
	Description string `json:"description"`
}

func main() {
	// Set Gin to release mode if not in development
	if os.Getenv("GIN_MODE") == "" {
		gin.SetMode(gin.ReleaseMode)
	}

	router := gin.Default()

	// Root endpoint with HTML
	router.GET("/", func(c *gin.Context) {
		c.Header("Content-Type", "text/html; charset=utf-8")
		c.String(http.StatusOK, `
<!DOCTYPE html>
<html>
<head>
    <title>Nixpacks + Go</title>
    <style>
        body {
            font-family: system-ui, sans-serif;
            max-width: 800px;
            margin: 0 auto;
            padding: 2rem;
            line-height: 1.6;
        }
        h1 { color: #00ADD8; }
        .info-box {
            background: #f5f5f5;
            padding: 1rem;
            border-radius: 8px;
            margin-top: 2rem;
        }
        a { color: #00ADD8; }
    </style>
</head>
<body>
    <h1>🐹 Hello from Nixpacks + Go!</h1>
    <p>This is a simple Gin web server deployed using Nixpacks auto-detection.</p>
    <div class="info-box">
        <h2>Deployment Info</h2>
        <ul>
            <li><strong>Framework:</strong> Gin</li>
            <li><strong>Language:</strong> Go</li>
            <li><strong>Deployed with:</strong> Nixpacks</li>
            <li><strong>Status:</strong> ✅ Running</li>
        </ul>
    </div>
    <div style="margin-top: 1rem;">
        <a href="/health">Check Health API →</a>
    </div>
</body>
</html>
        `)
	})

	// Health check endpoint
	router.GET("/health", func(c *gin.Context) {
		c.JSON(http.StatusOK, HealthResponse{
			Status:       "healthy",
			Framework:    "Gin",
			Version:      "1.9.1",
			DeployedWith: "nixpacks",
			GoVersion:    runtime.Version(),
		})
	})

	// Info endpoint
	router.GET("/info", func(c *gin.Context) {
		c.JSON(http.StatusOK, InfoResponse{
			Name:        "simple-go",
			Version:     "1.0.0",
			Description: "Simple Go/Gin app for Nixpacks testing",
		})
	})

	// Get port from environment or use default
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	addr := fmt.Sprintf("0.0.0.0:%s", port)
	fmt.Printf("Server starting on http://%s\n", addr)

	// Run server
	if err := router.Run(addr); err != nil {
		panic(fmt.Sprintf("Failed to start server: %v", err))
	}
}

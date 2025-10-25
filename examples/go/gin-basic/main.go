package main

import (
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
)

type Response struct {
	Message string `json:"message"`
	Status  string `json:"status"`
	Version string `json:"version"`
}

func main() {
	r := gin.Default()

	r.GET("/", func(c *gin.Context) {
		c.JSON(http.StatusOK, Response{
			Message: "Hello from Go Gin!",
			Status:  "healthy",
			Version: "1.0.0",
		})
	})

	r.GET("/health", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"status": "ok",
		})
	})

	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	r.Run("0.0.0.0:" + port)
}

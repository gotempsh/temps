// Sample Go app demonstrating Temps error tracking with source context.
//
// Deploy this to Temps, enable "Error Tracking Source Context" in the project
// settings, and hit GET /boom — the error shows up in Temps error tracking with
// the actual source code around each stack frame.
package main

import (
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/getsentry/sentry-go"
)

func main() {
	// SENTRY_DSN is auto-injected by Temps when error tracking is configured.
	//
	// Note what is DELIBERATELY not set: Release and Environment. sentry-go reads
	// SENTRY_RELEASE / SENTRY_ENVIRONMENT from the environment when they are empty,
	// and Temps injects SENTRY_RELEASE with the deployed commit SHA (see ADR-033).
	// Hard-coding Release here would make the SDK ignore the injected value and
	// break the source-context join. AttachStacktrace makes plain errors carry a
	// stack trace so the frames (and their source) are available.
	if dsn := os.Getenv("SENTRY_DSN"); dsn != "" {
		if err := sentry.Init(sentry.ClientOptions{
			Dsn:              dsn,
			AttachStacktrace: true,
		}); err != nil {
			log.Printf("sentry init failed: %v", err)
		}
		defer sentry.Flush(2 * time.Second)
	}

	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprintln(w, "ok — try GET /boom to send an error to Temps")
	})

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		fmt.Fprintln(w, "ok")
	})

	// /boom reports an error on purpose so you can see it — with source context —
	// in Temps error tracking.
	http.HandleFunc("/boom", func(w http.ResponseWriter, r *http.Request) {
		sentry.CaptureException(doWork())
		http.Error(w, "boom reported to Temps", http.StatusInternalServerError)
	})

	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	log.Printf("listening on :%s", port)
	log.Fatal(http.ListenAndServe("0.0.0.0:"+port, nil))
}

// doWork returns an error whose capture stack points back into this file, so the
// frames resolve to source once the source is uploaded to Temps.
func doWork() error {
	return fmt.Errorf("failed to do work: %w", errFromDeep())
}

func errFromDeep() error {
	return fmt.Errorf("something broke deep in the call stack")
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestClientWaitForRun(t *testing.T) {
	for _, terminal := range []string{"completed", "failed", "cancelled"} {
		t.Run(terminal, func(t *testing.T) {
			calls := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Header.Get("Authorization") != "Bearer test-token" {
					t.Error("missing authorization")
				}
				calls++
				status := "running"
				if calls == 2 {
					status = terminal
				}
				_, _ = w.Write([]byte(`{"id":"run-test","status":"` + status + `"}`))
			}))
			defer server.Close()
			run, err := NewClient(server.URL, "test-token").WaitForRun(context.Background(), "run-test", time.Second, time.Millisecond)
			if err != nil || run.Status != terminal {
				t.Fatalf("run = %v, err = %v", run, err)
			}
		})
	}
}

func TestClientWaitDeadline(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"status":"running"}`))
	}))
	defer server.Close()
	client := NewClient(server.URL, "test-token")
	_, err := client.WaitForRun(context.Background(), "run-test", 20*time.Millisecond, time.Hour)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("deadline: %v", err)
	}
	for _, value := range []time.Duration{0, -1} {
		if _, err := client.WaitForRun(context.Background(), "run-test", value, time.Second); err == nil {
			t.Fatal("accepted invalid timeout")
		}
		if _, err := client.WaitForRun(context.Background(), "run-test", time.Second, value); err == nil {
			t.Fatal("accepted invalid interval")
		}
	}
}

func TestClientWaitCancelsInflightRequest(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-r.Context().Done()
	}))
	defer server.Close()
	_, err := NewClient(server.URL, "test-token").WaitForRun(context.Background(), "run-test", 20*time.Millisecond, time.Millisecond)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("in-flight deadline: %v", err)
	}
}

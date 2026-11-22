// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

type APIError struct {
	Status int
	Title  string
}

func (e *APIError) Error() string {
	return fmt.Sprintf("workflow API: %s (HTTP %d)", e.Title, e.Status)
}

type HTTPTransport struct {
	APIURL string
	Client *http.Client
}

func (t *HTTPTransport) Call(ctx context.Context, method, path, token string, body, out any, retry bool) error {
	encoded, err := json.Marshal(body)
	if err != nil {
		return errors.New("workflow request is not JSON serializable")
	}
	client := &http.Client{Timeout: 30 * time.Second}
	if t.Client != nil {
		copy := *t.Client
		client = &copy
		if client.Timeout == 0 {
			client.Timeout = 30 * time.Second
		}
	}
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	attempts := 1
	if retry {
		attempts = 4
	}
	var last error
	for attempt := 0; attempt < attempts; attempt++ {
		var reader io.Reader
		if body != nil {
			reader = bytes.NewReader(encoded)
		}
		req, err := http.NewRequestWithContext(ctx, method, strings.TrimRight(t.APIURL, "/")+path, reader)
		if err != nil {
			return errors.New("invalid workflow API URL")
		}
		req.Header.Set("Authorization", "Bearer "+token)
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("temps-workflows-protocol", "1")
		response, err := client.Do(req)
		if err != nil {
			last = &APIError{0, "unavailable"}
		} else {
			limit := int64(128 * 1024 * 1024)
			if response.StatusCode >= 300 {
				limit = 65536
			}
			data, readErr := io.ReadAll(io.LimitReader(response.Body, limit+1))
			response.Body.Close()
			if readErr != nil {
				last = &APIError{0, "response interrupted"}
			} else if int64(len(data)) > limit {
				last = &APIError{502, "response too large"}
			} else if response.StatusCode >= 200 && response.StatusCode < 300 {
				if out == nil {
					return nil
				}
				if err := json.Unmarshal(data, out); err != nil {
					return &APIError{502, "invalid JSON response"}
				}
				return nil
			} else {
				var problem struct {
					Title string `json:"title"`
				}
				_ = json.Unmarshal(data, &problem)
				last = &APIError{response.StatusCode, problem.Title}
				if response.StatusCode != 502 && response.StatusCode != 503 && response.StatusCode != 504 {
					return last
				}
			}
		}
		if attempt+1 < attempts {
			timer := time.NewTimer(time.Duration(250*(1<<attempt)) * time.Millisecond)
			select {
			case <-ctx.Done():
				timer.Stop()
				return ctx.Err()
			case <-timer.C:
			}
		}
	}
	return last
}

type Client struct {
	Transport Transport
	Token     string
}

func NewClient(apiURL, token string) *Client { return &Client{&HTTPTransport{APIURL: apiURL}, token} }
func (c *Client) Start(ctx context.Context, workflow string, input any, idempotencyKey string) (StartedRun, error) {
	var result StartedRun
	err := c.Transport.Call(ctx, "POST", "/workflows/v1/runs", c.Token, struct {
		Workflow string `json:"workflow"`
		Input    any    `json:"input"`
		Key      string `json:"idempotency_key,omitempty"`
	}{workflow, input, idempotencyKey}, &result, false)
	return result, err
}
func (c *Client) GetRun(ctx context.Context, id string) (Run, error) {
	var run Run
	err := c.Transport.Call(ctx, "GET", "/workflows/v1/runs/"+url.PathEscape(id), c.Token, nil, &run, false)
	return run, err
}
func (c *Client) SendEvent(ctx context.Context, event Event) error {
	return c.Transport.Call(ctx, "POST", "/workflows/v1/events", c.Token, event, nil, false)
}

// Target is a destination with an explicit incoming grant. It grants no run reads.
type Target struct{ ProjectID, EnvironmentID int }

func (t Target) path(operation string) (string, error) {
	if t.ProjectID <= 0 || t.EnvironmentID <= 0 {
		return "", fmt.Errorf("workflow target IDs must be positive integers")
	}
	return fmt.Sprintf("/workflows/v1/projects/%d/environments/%d/%s", t.ProjectID, t.EnvironmentID, operation), nil
}
func (c *Client) StartIn(ctx context.Context, target Target, workflow string, input any, idempotencyKey string) (StartedRun, error) {
	var result StartedRun
	path, err := target.path("runs")
	if err != nil {
		return result, err
	}
	err = c.Transport.Call(ctx, "POST", path, c.Token, struct {
		Workflow string `json:"workflow"`
		Input    any    `json:"input"`
		Key      string `json:"idempotency_key,omitempty"`
	}{workflow, input, idempotencyKey}, &result, false)
	return result, err
}
func (c *Client) SendEventTo(ctx context.Context, target Target, event Event) error {
	path, err := target.path("events")
	if err != nil {
		return err
	}
	return c.Transport.Call(ctx, "POST", path, c.Token, event, nil, false)
}

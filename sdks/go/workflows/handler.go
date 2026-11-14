// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"time"
)

type InvocationKey struct {
	Algorithm     string `json:"algorithm"`
	PublicKey     string `json:"public_key"`
	ProjectID     int    `json:"project_id"`
	EnvironmentID int    `json:"environment_id"`
}
type claims struct {
	Version       int    `json:"v"`
	Type          string `json:"typ"`
	ProjectID     int    `json:"pid"`
	EnvironmentID int    `json:"eid"`
	Run           string `json:"run"`
	Lease         int64  `json:"lease"`
	Expires       int64  `json:"exp"`
	Body          string `json:"body"`
}
type Handler struct {
	workflows map[string]Workflow
	key       InvocationKey
	public    ed25519.PublicKey
	transport Transport
}

// NewHandler reads the environment-bound key with the app credential at startup.
// Recreate the handler after an instance signing-key rotation, or use NewPinnedHandler.
func NewHandler(ctx context.Context, workflows []Workflow, client *Client) (*Handler, error) {
	var key InvocationKey
	if err := client.Transport.Call(ctx, "GET", "/workflows/v1/invocation-key", client.Token, nil, &key, true); err != nil {
		return nil, err
	}
	return NewPinnedHandler(workflows, key, client.Transport)
}
func NewPinnedHandler(workflows []Workflow, key InvocationKey, transport Transport) (*Handler, error) {
	public, err := base64.RawURLEncoding.DecodeString(key.PublicKey)
	if err != nil || len(public) != ed25519.PublicKeySize || key.Algorithm != "ed25519" {
		return nil, errors.New("invalid workflow invocation public key")
	}
	if transport == nil {
		return nil, errors.New("workflow transport is required")
	}
	handler := &Handler{map[string]Workflow{}, key, public, transport}
	for _, workflow := range workflows {
		if err := validName(workflow.Name, 128); err != nil {
			return nil, err
		}
		if workflow.Run == nil {
			return nil, errors.New("workflow function is required")
		}
		if workflow.Concurrency != nil && (*workflow.Concurrency < 1 || *workflow.Concurrency > 1000000) {
			return nil, errors.New("workflow concurrency must be in 1..1000000")
		}
		if _, ok := handler.workflows[workflow.Name]; ok {
			return nil, errors.New("duplicate workflow name")
		}
		handler.workflows[workflow.Name] = workflow
	}
	return handler, nil
}
func (h *Handler) verify(token string) (claims, error) {
	var c claims
	if len(token) > 8192 {
		return c, errors.New("invalid invocation token")
	}
	parts := strings.Split(token, ".")
	if len(parts) != 3 || parts[0] != "twi1" {
		return c, errors.New("invalid invocation token")
	}
	signature, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil || !ed25519.Verify(h.public, []byte(parts[0]+"."+parts[1]), signature) {
		return c, errors.New("invalid invocation token")
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil || json.Unmarshal(payload, &c) != nil || c.Version != 1 || c.Expires <= time.Now().Unix() || c.ProjectID != h.key.ProjectID || c.EnvironmentID != h.key.EnvironmentID {
		return c, errors.New("invalid invocation token")
	}
	return c, nil
}
func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	invoke := r.URL.Path == "/.well-known/temps/workflows/invoke"
	manifest := r.URL.Path == "/.well-known/temps/workflows/manifest"
	if !invoke && !manifest {
		respond(w, 404, map[string]string{"title": "Workflow endpoint not found"})
		return
	}
	method := "GET"
	kind := "manifest"
	if invoke {
		method = "POST"
		kind = "invoke"
	}
	if r.Method != method {
		respond(w, 405, map[string]string{"title": "Method not allowed"})
		return
	}
	auth := strings.Fields(r.Header.Get("Authorization"))
	if len(auth) != 2 || !strings.EqualFold(auth[0], "Bearer") {
		respond(w, 401, map[string]string{"title": "Invalid invocation token"})
		return
	}
	c, err := h.verify(auth[1])
	if err != nil || c.Type != kind {
		respond(w, 401, map[string]string{"title": "Invalid invocation token"})
		return
	}
	if manifest {
		workflows := make([]Workflow, 0, len(h.workflows))
		for _, workflow := range h.workflows {
			workflows = append(workflows, workflow)
		}
		respond(w, 200, struct {
			Version   int        `json:"protocol_version"`
			SDK       string     `json:"sdk"`
			Workflows []Workflow `json:"workflows"`
		}{1, "temps-workflows-go/0.1.0", workflows})
		return
	}
	_ = http.NewResponseController(w).SetReadDeadline(time.Now().Add(30 * time.Second))
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 64*1024*1024))
	if err != nil {
		respond(w, 413, map[string]string{"title": "Invocation body exceeds limit or could not be read"})
		return
	}
	digest := sha256.Sum256(body)
	if c.Body != hex.EncodeToString(digest[:]) {
		respond(w, 401, map[string]string{"title": "Invocation body does not match token"})
		return
	}
	var request Invocation
	if json.Unmarshal(body, &request) != nil || request.Version != 1 || request.Deadline.IsZero() || request.Now.IsZero() {
		respond(w, 400, map[string]string{"title": "Invalid invocation request"})
		return
	}
	if c.Run != request.RunID || c.Lease != request.Lease {
		respond(w, 401, map[string]string{"title": "Invocation run or lease does not match token"})
		return
	}
	workflow, ok := h.workflows[request.Workflow]
	if !ok {
		respond(w, 200, struct {
			Outcome Outcome `json:"outcome"`
		}{Outcome{Type: "failed", Error: "Workflow is not defined in this deployment"}})
		return
	}
	status, outcome := RunInvocation(r.Context(), workflow, request, auth[1], h.transport)
	respond(w, status, struct {
		Outcome Outcome `json:"outcome"`
	}{outcome})
}
func respond(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Cache-Control", "no-store")
	body, err := json.Marshal(value)
	if err != nil {
		status = 500
		body = []byte(`{"title":"Workflow result is not JSON serializable"}`)
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("temps-workflows-protocol", "1")
	w.WriteHeader(status)
	_, _ = w.Write(body)
}

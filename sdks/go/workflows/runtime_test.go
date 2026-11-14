// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

type recorder struct {
	entries []Entry
	outage  bool
}

func (r *recorder) Call(_ context.Context, method, path, token string, body, out any, retry bool) error {
	if r.outage {
		return &APIError{503, "unavailable"}
	}
	var response any
	if r.entries == nil {
		r.entries = []Entry{}
	}
	if strings.Contains(path, "/journal?") || strings.HasSuffix(path, "/admit") {
		response = struct {
			Entries []Entry `json:"entries"`
			Next    *int    `json:"next_sequence"`
		}{r.entries, nil}
	} else {
		write := body.(Write)
		r.entries = append(r.entries, write.Entry)
		response = struct {
			Entry Entry `json:"entry"`
		}{write.Entry}
	}
	data, err := json.Marshal(response)
	if err != nil {
		return err
	}
	return json.Unmarshal(data, out)
}
func invocation() Invocation {
	now := time.Now()
	return Invocation{Version: 1, RunID: "synthetic-run", Workflow: "orders", Attempt: 1, Lease: 1, Now: now, Deadline: now.Add(10 * time.Minute), LeaseExpires: now.Add(2 * time.Minute)}
}
func TestReplayPagesAndMismatch(t *testing.T) {
	transport := &recorder{}
	calls := 0
	workflow := Workflow{Name: "orders", Run: func(c *Context) (any, error) {
		return c.Step("charge", func(context.Context) (any, error) { calls++; return "receipt", nil })
	}}
	status, result := RunInvocation(context.Background(), workflow, invocation(), "token", transport)
	if status != 200 || result.Output != "receipt" {
		t.Fatalf("unexpected result: %d %#v", status, result)
	}
	request := invocation()
	zero := 0
	request.Next = &zero
	_, result = RunInvocation(context.Background(), workflow, request, "token", transport)
	if calls != 1 || result.Output != "receipt" {
		t.Fatal("replay repeated a completed step")
	}
	transport.entries[0].Name = "old-name"
	_, result = RunInvocation(context.Background(), workflow, request, "token", transport)
	if result.Type != "incompatible" {
		t.Fatal("mismatch was not detected")
	}
}
func TestSuspensionAndOutageCannotBeSwallowed(t *testing.T) {
	workflow := Workflow{Name: "orders", Run: func(c *Context) (any, error) { _ = c.Sleep("nap", time.Hour); return "must not finish", nil }}
	transport := &recorder{}
	_, result := RunInvocation(context.Background(), workflow, invocation(), "token", transport)
	if result.Type != "sleep" {
		t.Fatal("suspension swallowed")
	}
	transport.outage = true
	status, _ := RunInvocation(context.Background(), workflow, invocation(), "token", transport)
	if status != 503 {
		t.Fatal("outage became a workflow failure")
	}
}
func TestRetryAndFailedJournal(t *testing.T) {
	workflow := Workflow{Name: "orders", Run: func(c *Context) (any, error) {
		return c.Step("charge", func(context.Context) (any, error) { return nil, errors.New("synthetic failure") }, StepOptions{Retries: 1})
	}}
	transport := &recorder{}
	_, result := RunInvocation(context.Background(), workflow, invocation(), "token", transport)
	if result.Type != "retry" {
		t.Fatal("retry missing")
	}
	request := invocation()
	request.Journal = append([]Entry{}, transport.entries...)
	_, result = RunInvocation(context.Background(), workflow, request, "token", transport)
	if result.Type != "failed" {
		t.Fatal("retry did not end")
	}
}
func TestSignedHandlerChecksScopeBodyExpiryAndLease(t *testing.T) {
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	key := InvocationKey{"ed25519", base64.RawURLEncoding.EncodeToString(public), 1, 2}
	calls := 0
	handler, err := NewPinnedHandler([]Workflow{{Name: "orders", Run: func(c *Context) (any, error) { calls++; return c.Input, nil }}}, key, &recorder{})
	if err != nil {
		t.Fatal(err)
	}
	request := invocation()
	body, _ := json.Marshal(request)
	digest := sha256.Sum256(body)
	original := claims{1, "invoke", 1, 2, request.RunID, 1, time.Now().Unix() + 60, hex.EncodeToString(digest[:])}
	send := func(c claims) int {
		payload, _ := json.Marshal(c)
		message := "twi1." + base64.RawURLEncoding.EncodeToString(payload)
		token := message + "." + base64.RawURLEncoding.EncodeToString(ed25519.Sign(private, []byte(message)))
		r := httptest.NewRequest("POST", "/.well-known/temps/workflows/invoke", strings.NewReader(string(body)))
		r.Header.Set("Authorization", "Bearer "+token)
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, r)
		if w.Header().Get("Cache-Control") != "no-store" {
			t.Fatal("missing no-store")
		}
		return w.Code
	}
	for _, change := range []func(*claims){func(c *claims) { c.ProjectID = 3 }, func(c *claims) { c.EnvironmentID = 4 }, func(c *claims) { c.Expires = 0 }, func(c *claims) { c.Body = "wrong" }, func(c *claims) { c.Lease = 2 }, func(c *claims) { c.Type = "manifest" }} {
		c := original
		change(&c)
		if send(c) != 401 {
			t.Fatal("invalid token accepted")
		}
	}
	if calls != 0 {
		t.Fatal("unauthorized user code executed")
	}
	if send(original) != 200 || calls != 1 {
		t.Fatal("valid invocation rejected")
	}
}
func TestTransportDoesNotForwardCredentialsOnRedirect(t *testing.T) {
	reached := false
	destination := httptest.NewServer(httpHandler(func() { reached = true }))
	defer destination.Close()
	server := httptest.NewServer(redirectHandler(destination.URL))
	defer server.Close()
	transport := &HTTPTransport{APIURL: server.URL}
	err := transport.Call(context.Background(), "GET", "/key", "synthetic-token", nil, nil, false)
	if err == nil || reached {
		t.Fatal("redirect followed")
	}
}

func httpHandler(fn func()) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { fn(); w.WriteHeader(200) })
}
func redirectHandler(url string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { http.Redirect(w, r, url, 302) })
}

func TestGrantTargetPaths(t *testing.T) {
	target := Target{2, 4}
	path, err := target.path("runs")
	if err != nil || path != "/workflows/v1/projects/2/environments/4/runs" {
		t.Fatalf("unexpected target: %s %v", path, err)
	}
	if _, err = (Target{-1, 4}).path("events"); err == nil {
		t.Fatal("invalid target accepted")
	}
}

func TestUnreachedJournalPagesAreNotFetched(t *testing.T) {
	request := invocation()
	request.Journal = []Entry{{Sequence: 0, Name: "first", Kind: "step", Status: "completed", Attempts: 1, Result: 7}}
	next := 1
	request.Next = &next
	transport := &admissionPageTransport{request.Journal, request.Next}
	workflow := Workflow{Name: "orders", Run: func(c *Context) (any, error) {
		value, err := c.Step("first", func(context.Context) (any, error) { t.Fatal("replay executed"); return nil, nil })
		if len(c.journal) != 0 {
			t.Fatal("consumed journal entries retained")
		}
		return value, err
	}}
	status, result := RunInvocation(context.Background(), workflow, request, "synthetic", transport)
	if status != 200 || result.Type != "incompatible" || result.Sequence == nil || *result.Sequence != 1 {
		t.Fatalf("unexpected result %d %+v", status, result)
	}
}

type admissionPageTransport struct {
	Entries []Entry
	Next    *int
}

func (a *admissionPageTransport) Call(_ context.Context, _, path, _ string, _, out any, retry bool) error {
	if !strings.HasSuffix(path, "/admit") || retry {
		return errors.New("unexpected API request")
	}
	data, _ := json.Marshal(map[string]any{"entries": a.Entries, "next_sequence": a.Next})
	return json.Unmarshal(data, out)
}

func TestRemovedWaitCannotComplete(t *testing.T) {
	for _, paged := range []bool{false, true} {
		req := invocation()
		if paged {
			zero := 0
			req.Next = &zero
		} else {
			req.Journal = []Entry{{Sequence: 0, Name: "approval", Kind: "wait", Status: "pending", Attempts: 1}}
		}
		status, result := RunInvocation(context.Background(), Workflow{Name: "orders", Run: func(c *Context) (any, error) { return "done", nil }}, req, "token", &recorder{entries: []Entry{{Sequence: 0, Name: "approval", Kind: "wait", Status: "pending", Attempts: 1}}})
		if status != 200 || result.Type != "incompatible" || result.Sequence == nil || *result.Sequence != 0 {
			t.Fatalf("removed wait completed: %d %#v", status, result)
		}
	}
}

type deniedAdmission struct {
	status int
	calls  int
}

func (d *deniedAdmission) Call(_ context.Context, _, path, _ string, _, _ any, retry bool) error {
	d.calls++
	if !strings.HasSuffix(path, "/admit") || retry {
		panic("admission must be one-shot")
	}
	return &APIError{d.status, "unavailable"}
}
func TestAdmissionFailsClosedBeforeEffects(t *testing.T) {
	for _, status := range []int{409, 503} {
		transport := &deniedAdmission{status: status}
		effects := 0
		workflow := Workflow{Name: "orders", Run: func(*Context) (any, error) { effects++; return nil, nil }}
		code, _ := RunInvocation(context.Background(), workflow, invocation(), "token", transport)
		expected := 503
		if status == 409 {
			expected = 202
		}
		if code != expected || effects != 0 || transport.calls != 1 {
			t.Fatalf("admission failed: %d %d %d", code, effects, transport.calls)
		}
	}
}

func TestMalformedAdmissionRunsNoUserCode(t *testing.T) {
	effects := 0
	workflow := Workflow{Name: "orders", Run: func(*Context) (any, error) { effects++; return nil, nil }}
	code, _ := RunInvocation(context.Background(), workflow, invocation(), "token", &admissionPageTransport{})
	if code != 503 || effects != 0 {
		t.Fatalf("malformed admission executed: %d %d", code, effects)
	}
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Loopback-only fixture server for the shared real-engine conformance suite.
package main

import (
	"context"
	"encoding/json"
	"errors"
	wf "github.com/gotempsh/temps/sdks/go/workflows"
	"log"
	"net/http"
	"os"
	"sync"
	"time"
)

var mu sync.Mutex
var effects = map[string]int{}
var variant = "a"
var gate = make(chan struct{})

func effect(key string) int { mu.Lock(); defer mu.Unlock(); effects[key]++; return effects[key] }
func counted(c *wf.Context, name, key string) (any, error) {
	return c.Step(name, func(context.Context) (any, error) { return effect(key), nil })
}
func replay(c *wf.Context) (any, error) {
	a, e := counted(c, "a", "conf-replay/a")
	if e != nil {
		return nil, e
	}
	if e = c.Sleep("pause", 1500*time.Millisecond); e != nil {
		return nil, e
	}
	b, e := counted(c, "b", "conf-replay/b")
	if e != nil {
		return nil, e
	}
	now, e := c.Now("now")
	if e != nil {
		return nil, e
	}
	random, e := c.Random("random")
	if e != nil {
		return nil, e
	}
	parsed, e := time.Parse(time.RFC3339Nano, now.(string))
	if e != nil {
		return nil, e
	}
	return map[string]any{"a": a, "b": b, "now": parsed.UTC().Format("2006-01-02T15:04:05.000Z"), "random": random}, nil
}
func retry(c *wf.Context) (any, error) {
	return c.Step("flaky", func(context.Context) (any, error) {
		n := effect("conf-retry/flaky")
		if n < 3 {
			return nil, errors.New("upstream failed")
		}
		return map[string]any{"attempt": n}, nil
	}, wf.StepOptions{Retries: 3, InitialBackoff: time.Second})
}
func exhausted(c *wf.Context) (any, error) {
	return c.Step("always-fails", func(context.Context) (any, error) {
		effect("conf-retry-exhausted/always-fails")
		return nil, errors.New("the upstream is down")
	}, wf.StepOptions{Retries: 1, InitialBackoff: time.Second})
}
func suspend(c *wf.Context) (any, error) {
	input := c.Input.(map[string]any)
	key := input["key"].(string)
	timeout := time.Hour
	if value, ok := input["timeoutMs"].(float64); ok {
		timeout = time.Duration(value) * time.Millisecond
	}
	approval, e := c.WaitForEvent("approval", "conf.approved", &key, timeout)
	if e != nil {
		return nil, e
	}
	after, e := counted(c, "after", "conf-suspend/after")
	return map[string]any{"approval": approval, "after": after}, e
}
func incompatible(c *wf.Context) (any, error) {
	mu.Lock()
	name := "one"
	if variant == "b" {
		name = "uno"
	}
	mu.Unlock()
	if _, e := counted(c, name, "conf-incompatible/"+name); e != nil {
		return nil, e
	}
	return "done", c.Sleep("gap", 1500*time.Millisecond)
}
func gated(c *wf.Context) (any, error) {
	return c.Step("gated", func(ctx context.Context) (any, error) {
		entered := effect("conf-gated/entered")
		mu.Lock()
		wait := gate
		mu.Unlock()
		select {
		case <-wait:
			return map[string]any{"entered": entered}, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	})
}
func idempotent(c *wf.Context) (any, error) { return counted(c, "once", "conf-idempotent/once") }
func parent(c *wf.Context) (any, error) {
	before, e := counted(c, "before", "conf-parent/before")
	if e != nil {
		return nil, e
	}
	child, e := c.Child("square", "conf-child", c.Input)
	return map[string]any{"before": before, "child": child}, e
}
func child(c *wf.Context) (any, error) {
	return c.Step("square", func(context.Context) (any, error) {
		effect("conf-child/square")
		input := c.Input.(map[string]any)
		if input["fail"] == true {
			return nil, errors.New("the child cannot square that")
		}
		n := input["n"].(float64)
		return n * n, nil
	}, wf.StepOptions{Retries: 0})
}
func main() {
	client := wf.NewClient(os.Getenv("TEMPS_API_URL"), os.Getenv("TEMPS_WORKFLOWS_TOKEN"))
	definitions := []wf.Workflow{{Name: "conf-replay", Run: replay}, {Name: "conf-retry", Run: retry}, {Name: "conf-retry-exhausted", Run: exhausted}, {Name: "conf-suspend", Run: suspend}, {Name: "conf-incompatible", Run: incompatible}, {Name: "conf-gated", Run: gated}, {Name: "conf-idempotent", Run: idempotent}, {Name: "conf-parent", Run: parent}, {Name: "conf-child", Run: child}}
	handler, e := wf.NewHandler(context.Background(), definitions, client)
	if e != nil {
		log.Fatal(e)
	}
	mux := http.NewServeMux()
	mux.Handle("/", handler)
	mux.HandleFunc("/__fixture/", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		switch r.URL.Path {
		case "/__fixture/reset":
			effects = map[string]int{}
			variant = "a"
			gate = make(chan struct{})
		case "/__fixture/variant/b":
			variant = "b"
		case "/__fixture/variant/a":
			variant = "a"
		case "/__fixture/gate":
			select {
			case <-gate:
			default:
				close(gate)
			}
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(effects)
	})
	server := http.Server{Addr: "127.0.0.1:" + os.Getenv("EXECUTOR_PORT"), Handler: mux, ReadHeaderTimeout: 5 * time.Second}
	log.Fatal(server.ListenAndServe())
}

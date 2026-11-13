// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"net/url"
	"sync"
	"time"
	"unicode/utf8"
)

type signal struct {
	outcome Outcome
	status  int
}

func (s *signal) Error() string { return "workflow invocation suspended" }

type Context struct {
	context.Context
	Input     any
	RunID     string
	Workflow  string
	Attempt   int
	request   Invocation
	transport Transport
	token     string
	journal   map[int]Entry
	sequence  int
	cursor    *int
	started   time.Time
	mu        sync.Mutex
	control   *signal
}

func (c *Context) clock() time.Time { return c.request.Now.Add(time.Since(c.started)) }
func (c *Context) stop(outcome Outcome, status int) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.control == nil {
		c.control = &signal{outcome, status}
	}
	return c.control
}
func (c *Context) stopped() *signal { c.mu.Lock(); defer c.mu.Unlock(); return c.control }
func validName(name string, max int) error {
	if name == "" || utf8.RuneCountInString(name) > max {
		return fmt.Errorf("workflow name must contain 1..%d characters", max)
	}
	return nil
}
func (c *Context) next(name, kind string) (int, *Entry, error) {
	if err := validName(name, 200); err != nil {
		return 0, nil, err
	}
	if signal := c.stopped(); signal != nil {
		return 0, nil, signal
	}
	seq := c.sequence
	c.sequence++

	for c.cursor != nil && seq >= *c.cursor {
		cursor := *c.cursor
		var page struct {
			Entries []Entry `json:"entries"`
			Next    *int    `json:"next_sequence"`
		}
		if err := c.call("GET", fmt.Sprintf("/journal?from=%d", cursor), nil, &page); err != nil {
			return seq, nil, err
		}
		if page.Next != nil && (*page.Next <= cursor || *page.Next > 100000) {
			return seq, nil, c.stop(Outcome{}, 503)
		}
		c.journal = map[int]Entry{}
		for _, entry := range page.Entries {
			c.journal[entry.Sequence] = entry
		}
		c.cursor = page.Next
	}
	if entry, ok := c.journal[seq]; ok {
		delete(c.journal, seq)
		if entry.Name != name || entry.Kind != kind {
			return seq, nil, c.stop(Outcome{Type: "incompatible", Sequence: &seq, Expected: entry.Kind + " '" + entry.Name + "'", Found: kind + " '" + name + "'"}, 200)
		}
		return seq, &entry, nil
	}
	return seq, nil, nil
}
func (c *Context) call(method, suffix string, body, out any) error {
	err := c.transport.Call(c.Context, method, "/workflow-runs/"+url.PathEscape(c.RunID)+suffix, c.token, body, out, true)
	var api *APIError
	if errors.As(err, &api) {
		if api.Status == 409 {
			return c.stop(Outcome{}, 202)
		}
		if api.Status == 0 || api.Status >= 500 {
			return c.stop(Outcome{}, 503)
		}
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return c.stop(Outcome{}, 503)
	}
	return err
}
func (c *Context) record(write Write) (Entry, error) {
	var reply struct {
		Entry Entry `json:"entry"`
	}
	if err := c.call("POST", "/steps", write, &reply); err != nil {
		return Entry{}, err
	}
	return reply.Entry, nil
}

// StepError identifies a failed entry that was durably recorded by Temps.
type StepError struct {
	Sequence int
	Name     string
	Message  string
}

func (e *StepError) Error() string {
	return fmt.Sprintf("Workflow step '%s' failed: %s", e.Name, e.Message)
}
func result(entry Entry) (any, error) {
	if entry.Status == "failed" {
		message := "recorded failure"
		if entry.Error != nil {
			message = *entry.Error
		}
		return nil, &StepError{entry.Sequence, entry.Name, message}
	}
	return entry.Result, nil
}

type StepOptions struct {
	Retries        int
	Timeout        time.Duration
	InitialBackoff time.Duration
	MaxBackoff     time.Duration
}

func (c *Context) Step(name string, fn func(context.Context) (any, error), options ...StepOptions) (any, error) {
	seq, entry, err := c.next(name, "step")
	if err != nil {
		return nil, err
	}
	if entry != nil && entry.Status != "pending" {
		return result(*entry)
	}
	margin := 30 * time.Second
	if quarter := c.request.Deadline.Sub(c.request.Now) / 4; quarter < margin {
		margin = quarter
	}
	if !c.clock().Before(c.request.Deadline.Add(-margin)) {
		at := c.clock()
		return nil, c.stop(Outcome{Type: "retry", Sequence: &seq, At: &at}, 200)
	}
	opt := StepOptions{Retries: 3, InitialBackoff: time.Second, MaxBackoff: 300 * time.Second}
	if len(options) > 0 {
		opt = options[0]
	}
	if opt.InitialBackoff <= 0 {
		opt.InitialBackoff = time.Second
	}
	if opt.MaxBackoff <= 0 {
		opt.MaxBackoff = 300 * time.Second
	}
	attempt := 1
	if entry != nil {
		attempt = entry.Attempts + 1
	}
	ctx := c.Context
	cancel := func() {}
	if opt.Timeout > 0 {
		ctx, cancel = context.WithTimeout(ctx, opt.Timeout)
	}
	defer cancel()
	started := time.Now()
	value, runErr := fn(ctx)
	if runErr == nil && ctx.Err() != nil {
		runErr = ctx.Err()
	}
	if c.stopped() != nil {
		return nil, c.stopped()
	}
	write := Write{Entry: Entry{Sequence: seq, Name: name, Kind: "step", Status: "completed", Attempts: attempt, Result: value}, Duration: time.Since(started).Milliseconds()}
	if runErr != nil {
		message := clip(runErr.Error())
		write.Error = &message
		write.Result = nil
		write.Status = "pending"
		if attempt > opt.Retries {
			write.Status = "failed"
		}
	}
	saved, err := c.record(write)
	if err != nil {
		return nil, err
	}
	if saved.Status != "pending" {
		return result(saved)
	}
	backoff := opt.InitialBackoff
	for i := 1; i < attempt && backoff < opt.MaxBackoff; i++ {
		if backoff > opt.MaxBackoff/2 {
			backoff = opt.MaxBackoff
		} else {
			backoff *= 2
		}
	}
	if backoff > opt.MaxBackoff {
		backoff = opt.MaxBackoff
	}
	at := c.clock().Add(backoff)
	return nil, c.stop(Outcome{Type: "retry", Sequence: &seq, At: &at}, 200)
}
func (c *Context) Sleep(name string, duration time.Duration) error {
	return c.SleepUntil(name, c.clock().Add(duration))
}
func (c *Context) SleepUntil(name string, until time.Time) error {
	seq, entry, err := c.next(name, "sleep")
	if err != nil {
		return err
	}
	if entry != nil && entry.Status == "completed" {
		return nil
	}
	if entry != nil && entry.WakeAt != nil {
		until = *entry.WakeAt
	}
	if !c.clock().Before(until) {
		_, err = c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "sleep", Status: "completed", Attempts: 1}})
		return err
	}
	if entry == nil {
		_, err = c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "sleep", Status: "pending", Attempts: 1, WakeAt: &until}})
		if err != nil {
			return err
		}
	}
	return c.stop(Outcome{Type: "sleep", Sequence: &seq, Until: &until}, 200)
}
func (c *Context) WaitForEvent(name, event string, key *string, timeout time.Duration) (any, error) {
	if err := eventName(event); err != nil {
		return nil, err
	}
	seq, entry, err := c.next(name, "wait")
	if err != nil {
		return nil, err
	}
	if entry != nil && entry.Status == "completed" {
		return entry.Result, nil
	}
	var until *time.Time
	if entry != nil {
		until = entry.WakeAt
	} else if timeout > 0 {
		t := c.clock().Add(timeout)
		until = &t
	}
	if entry == nil {
		_, err = c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "wait", Status: "pending", Attempts: 1, WakeAt: until}})
		if err != nil {
			return nil, err
		}
	} else if until != nil && !c.clock().Before(*until) {
		saved, err := c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "wait", Status: "completed", Attempts: entry.Attempts, Result: map[string]any{"received": false, "payload": nil}}})
		return saved.Result, err
	}
	return nil, c.stop(Outcome{Type: "wait", Sequence: &seq, Event: event, Key: key, TimeoutAt: until}, 200)
}
func eventName(event string) error {
	if err := validName(event, 200); err != nil {
		return err
	}
	if len(event) >= 6 && event[:6] == "temps." {
		return errors.New("temps. events are reserved")
	}
	return nil
}
func (c *Context) SendEvent(name string, event Event) error {
	if err := eventName(event.Name); err != nil {
		return err
	}
	seq, entry, err := c.next(name, "send_event")
	if err != nil {
		return err
	}
	if entry != nil {
		return nil
	}
	_, err = c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "send_event", Status: "completed", Attempts: 1}, Event: &event})
	return err
}
func (c *Context) Child(name, workflow string, input any) (any, error) {
	if err := validName(workflow, 128); err != nil {
		return nil, err
	}
	seq, entry, err := c.next(name, "child")
	if err != nil {
		return nil, err
	}
	if entry == nil {
		saved, err := c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "child", Status: "pending", Attempts: 1}, Child: &ChildStart{workflow, input}})
		if err != nil {
			return nil, err
		}
		entry = &saved
	}
	if entry.Status == "completed" {
		return entry.Result, nil
	}
	object, ok := entry.Result.(map[string]any)
	if !ok {
		return nil, errors.New("child journal entry has no run id")
	}
	id, ok := object["run_id"].(string)
	if !ok {
		return nil, errors.New("child journal entry has no run id")
	}
	return nil, c.stop(Outcome{Type: "wait", Sequence: &seq, Event: "temps.child", Key: &id}, 200)
}
func (c *Context) Now(name string) (any, error) {
	seq, entry, err := c.next(name, "now")
	if err != nil {
		return nil, err
	}
	if entry != nil {
		return entry.Result, nil
	}
	saved, err := c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "now", Status: "completed", Attempts: 1, Result: c.clock().UTC().Format(time.RFC3339Nano)}})
	return saved.Result, err
}
func (c *Context) Random(name string) (any, error) {
	seq, entry, err := c.next(name, "random")
	if err != nil {
		return nil, err
	}
	if entry != nil {
		return entry.Result, nil
	}
	var bits [8]byte
	if _, err = rand.Read(bits[:]); err != nil {
		return nil, err
	}
	saved, err := c.record(Write{Entry: Entry{Sequence: seq, Name: name, Kind: "random", Status: "completed", Attempts: 1, Result: float64(binary.BigEndian.Uint64(bits[:])>>11) / (1 << 53)}})
	return saved.Result, err
}
func clip(message string) string {
	runes := []rune(message)
	if len(runes) > 16000 {
		return string(runes[:16000])
	}
	return message
}

func RunInvocation(parent context.Context, workflow Workflow, request Invocation, token string, transport Transport) (int, Outcome) {
	ctx, cancel := context.WithTimeout(parent, request.Deadline.Sub(request.Now))
	defer cancel()
	c := &Context{Context: ctx, Input: request.Input, RunID: request.RunID, Workflow: request.Workflow, Attempt: request.Attempt, request: request, transport: transport, token: token, journal: map[int]Entry{}, started: time.Now()}
	for _, entry := range request.Journal {
		c.journal[entry.Sequence] = entry
	}
	c.cursor = request.Next
	done := make(chan struct{})
	go func() {
		defer close(done)
		interval := request.LeaseExpires.Sub(request.Now) / 3
		if interval < time.Second {
			interval = time.Second
		}
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				var reply struct {
					Cancelled bool `json:"cancelled"`
				}
				if err := c.call("POST", "/heartbeat", struct{}{}, &reply); err == nil && reply.Cancelled {
					_ = c.stop(Outcome{}, 202)
				}
				if c.stopped() != nil {
					cancel()
					return
				}
			}
		}
	}()
	output, err := workflow.Run(c)
	deadlineError := ctx.Err()
	cancel()
	<-done
	if s := c.stopped(); s != nil {
		return s.status, s.outcome
	}
	if parent.Err() != nil || deadlineError != nil {
		return 503, Outcome{}
	}
	if err != nil {
		var step *StepError
		var sequence *int
		if errors.As(err, &step) {
			sequence = &step.Sequence
		}
		return 200, Outcome{Type: "failed", Error: clip(err.Error()), Sequence: sequence}
	}
	return 200, Outcome{Type: "completed", Output: output}
}

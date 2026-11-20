// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Package workflows provides durable replay and an authenticated HTTP executor for Temps.
package workflows

import (
	"context"
	"encoding/json"
	"time"
)

type Entry struct {
	Sequence int        `json:"sequence"`
	Name     string     `json:"name"`
	Kind     string     `json:"kind"`
	Status   string     `json:"status"`
	Attempts int        `json:"attempts"`
	Result   any        `json:"result,omitempty"`
	Error    *string    `json:"error,omitempty"`
	WakeAt   *time.Time `json:"wake_at,omitempty"`
}
type Invocation struct {
	Version      int       `json:"protocol_version"`
	RunID        string    `json:"run_id"`
	Workflow     string    `json:"workflow"`
	Attempt      int       `json:"attempt"`
	Lease        int64     `json:"lease"`
	Now          time.Time `json:"now"`
	Deadline     time.Time `json:"deadline_at"`
	LeaseExpires time.Time `json:"lease_expires_at"`
	Input        any       `json:"input"`
	Journal      []Entry   `json:"journal"`
	Next         *int      `json:"journal_next_sequence"`
}
type Write struct {
	Entry
	Event    *Event      `json:"event,omitempty"`
	Child    *ChildStart `json:"child,omitempty"`
	Duration int64       `json:"duration_ms,omitempty"`
}
type Event struct {
	Name    string  `json:"name"`
	Key     *string `json:"key,omitempty"`
	Payload any     `json:"payload"`
}
type ChildStart struct {
	Workflow string `json:"workflow"`
	Input    any    `json:"input"`
}
type Outcome struct {
	Type      string     `json:"type"`
	Output    any        `json:"output,omitempty"`
	Error     string     `json:"error,omitempty"`
	Sequence  *int       `json:"sequence,omitempty"`
	Until     *time.Time `json:"until,omitempty"`
	At        *time.Time `json:"at,omitempty"`
	Event     string     `json:"event,omitempty"`
	Key       *string    `json:"key,omitempty"`
	TimeoutAt *time.Time `json:"timeout_at,omitempty"`
	Expected  string     `json:"expected,omitempty"`
	Found     string     `json:"found,omitempty"`
}
type Workflow struct {
	Name        string                      `json:"name"`
	Version     string                      `json:"version,omitempty"`
	Concurrency *int                        `json:"concurrency,omitempty"`
	Run         func(*Context) (any, error) `json:"-"`
}
type Transport interface {
	Call(context.Context, string, string, string, any, any, bool) error
}
type Run struct {
	ID       string          `json:"id"`
	Workflow string          `json:"workflow"`
	Status   string          `json:"status"`
	Output   json.RawMessage `json:"output"`
	Error    *string         `json:"error"`
}
type StartedRun struct {
	Run     Run  `json:"run"`
	Created bool `json:"created"`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// sentry-go v0.36.2, source 2aecd6ed8f56c837a4d4e6ad6cef8069ede88fec.
package main

import (
	"encoding/json"
	"errors"
	"fmt"
	sentry "github.com/getsentry/sentry-go"
	"os"
	"time"
)

func main() {
	err := sentry.Init(sentry.ClientOptions{Dsn: os.Getenv("SENTRY_DSN"), Release: "synthetic-release-2", Environment: "synthetic-test", AttachStacktrace: true,
		Integrations: func([]sentry.Integration) []sentry.Integration { return nil },
		BeforeSend: func(event *sentry.Event, _ *sentry.EventHint) *sentry.Event {
			event.ServerName = "synthetic.example"
			for i := range event.Exception {
				if event.Exception[i].Stacktrace != nil {
					for j := range event.Exception[i].Stacktrace.Frames {
						frame := &event.Exception[i].Stacktrace.Frames[j]
						frame.Filename = "synthetic/application.go"
						frame.AbsPath = "/synthetic/application.go"
						frame.Vars = nil
					}
				}
			}
			for i := range event.Threads {
				if event.Threads[i].Stacktrace != nil {
					for j := range event.Threads[i].Stacktrace.Frames {
						frame := &event.Threads[i].Stacktrace.Frames[j]
						frame.Filename = "synthetic/application.go"
						frame.AbsPath = "/synthetic/application.go"
						frame.Vars = nil
					}
				}
			}
			return event
		},
	})
	if err != nil {
		panic("SDK initialization failed")
	}
	prefix := os.Args[1]
	ids := []string{}
	sentry.ConfigureScope(func(scope *sentry.Scope) {
		scope.SetUser(sentry.User{ID: "synthetic-user", Email: "synthetic@example.invalid"})
		scope.SetTag("synthetic.run", prefix)
		scope.SetTag("synthetic.key", "test'value")
		scope.SetContext("synthetic", sentry.Context{"nullable": nil, "array": []any{1, true, nil}})
		scope.AddBreadcrumb(&sentry.Breadcrumb{Category: "synthetic", Message: "Synthetic interaction", Data: map[string]any{"count": 7}}, 20)
	})
	for index, fingerprint := range []string{"shared", "shared", "split"} {
		sentry.ConfigureScope(func(scope *sentry.Scope) { scope.SetFingerprint([]string{prefix, fingerprint}) })
		var err error = errors.New("Synthetic outer failure")
		if index == 1 {
			err = fmt.Errorf("Synthetic wrapped failure: %w", err)
		}
		id := sentry.CaptureException(err)
		if id == nil {
			panic("SDK emitted no error")
		}
		ids = append(ids, string(*id))
	}
	sentry.ConfigureScope(func(scope *sentry.Scope) { scope.SetFingerprint([]string{"{{ default }}", prefix, "message"}) })
	id := sentry.CaptureMessage("Synthetic SDK message")
	if id == nil {
		panic("SDK emitted no message")
	}
	ids = append(ids, string(*id))
	if sentry.CaptureEvent(&sentry.Event{EventID: sentry.EventID(ids[0]), Message: "Synthetic retry identity"}) == nil {
		panic("SDK emitted no retry")
	}
	if !sentry.Flush(10 * time.Second) {
		panic("SDK flush failed")
	}
	encoded, _ := json.Marshal(map[string]any{"sdk": sentry.SDKVersion, "ids": ids, "retry": true})
	fmt.Println(string(encoded))
}

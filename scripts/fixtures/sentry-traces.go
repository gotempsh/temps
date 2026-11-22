// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// sentry-go v0.36.2, revision 2aecd6ed8f56c837a4d4e6ad6cef8069ede88fec.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	sentry "github.com/getsentry/sentry-go"
	"os"
	"time"
)

func main() {
	if err := sentry.Init(sentry.ClientOptions{Dsn: os.Getenv("SENTRY_DSN"),
		Release: "synthetic-tracing-release", Environment: "synthetic-test", ServerName: "synthetic.example",
		EnableTracing: true, TracesSampleRate: 1,
		Integrations: func([]sentry.Integration) []sentry.Integration { return nil },
	}); err != nil {
		panic("SDK initialization failed")
	}
	root := sentry.StartTransaction(context.Background(), "Synthetic cross SDK root", sentry.WithOpName("http.server"))
	root.SetData("service.name", "synthetic-tracing-service")
	root.SetData("sentry.kind", 2)
	root.SetTag("synthetic.run", os.Args[1])
	root.Status = sentry.SpanStatusOK
	child := root.StartChild("db.query")
	child.Description = "Synthetic cross SDK child"
	child.SetData("synthetic.flag", true)
	child.SetData("sentry.kind", 3)
	child.SetTag("synthetic.run", os.Args[1])
	child.Status = sentry.SpanStatusInternalError
	time.Sleep(2 * time.Millisecond)
	child.Finish()
	root.Finish()
	if !sentry.Flush(10 * time.Second) {
		panic("SDK flush failed")
	}
	output, _ := json.Marshal(map[string]any{"sdk": sentry.SDKVersion, "trace_id": root.TraceID.String(), "root_id": root.SpanID.String(), "child_id": child.SpanID.String()})
	fmt.Println(string(output))
}

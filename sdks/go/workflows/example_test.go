// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

package workflows_test

import (
	"context"
	"fmt"
	workflows "github.com/gotempsh/temps/sdks/go/workflows"
	"os"
	"time"
)

func ExampleClient() {
	client := workflows.NewClient(os.Getenv("TEMPS_API_URL"), os.Getenv("TEMPS_WORKFLOWS_TOKEN"))
	started, err := client.Start(context.Background(), "hello", map[string]string{"name": "World"}, "hello-example-1")
	if err != nil {
		fmt.Println(err)
		return
	}
	run, err := client.WaitForRun(context.Background(), started.Run.ID, time.Minute, time.Second)
	if err != nil {
		fmt.Println(err)
		return
	}
	fmt.Println(run.Status, string(run.Output))
}

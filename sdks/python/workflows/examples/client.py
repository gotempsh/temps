# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Install temps-workflows; use an environment workflow credential."""
import asyncio
import os
from temps_workflows import Client


async def main():
    client = Client(os.environ['TEMPS_API_URL'], os.environ['TEMPS_WORKFLOWS_TOKEN'])
    started = await client.start('hello', {'name': 'World'}, idempotency_key='hello-example-1')
    run = await client.wait_for_run(started['run']['id'], timeout=60)
    print(run['status'], run.get('output'))


if __name__ == '__main__':
    asyncio.run(main())

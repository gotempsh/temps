# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise the startup deadline against a listener that never sends headers."""

import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest


class StartupDeadlineTests(unittest.TestCase):
    def test_connected_but_unresponsive_server(self):
        script = Path(__file__).with_name("temps-instance.sh")
        with socket.socket() as listener, tempfile.TemporaryDirectory() as work:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            result = subprocess.run(
                ["bash", str(script), "wait"],
                env=dict(os.environ, FIRST_RUN_DIR=work, FIRST_RUN_START_TIMEOUT="2",
                         FIRST_RUN_ADDRESS=f"127.0.0.1:{listener.getsockname()[1]}",
                         NO_PROXY="127.0.0.1", no_proxy="127.0.0.1"),
                capture_output=True, text=True, timeout=6,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("within 2s", result.stderr)


if __name__ == "__main__":
    unittest.main()

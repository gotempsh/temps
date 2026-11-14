# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import unittest
import temps_plugin
from temps_plugin.client import TempsClient


class PackageTests(unittest.TestCase):
    def test_public_imports(self):
        for name in temps_plugin.__all__:
            self.assertIsNotNone(getattr(temps_plugin, name))
        self.assertTrue(callable(TempsClient))

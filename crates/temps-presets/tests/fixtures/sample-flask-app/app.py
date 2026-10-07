# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

from flask import Flask

app = Flask(__name__)


@app.get("/")
def index():
    return "sample-flask-ok"

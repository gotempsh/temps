#!/bin/bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Build WASM and create inline version for challenge.html

# Build WASM
npm run build

# Convert WASM to base64 and create JavaScript constant
echo "// Auto-generated WASM module (base64 encoded)" > pkg/wasm_inline.js
echo "const WASM_BASE64 = '" >> pkg/wasm_inline.js
base64 -i pkg/temps_captcha_wasm_bg.wasm >> pkg/wasm_inline.js
echo "';" >> pkg/wasm_inline.js

echo "WASM inline module created at pkg/wasm_inline.js"

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from 'vitest/config';
export default defineConfig({
    test: {
        globals: true,
        environment: 'node',
        coverage: {
            provider: 'v8',
            reporter: ['text', 'json', 'html'],
            exclude: [
                'node_modules/',
                'src/client/**', // Exclude generated client code
                '*.config.ts',
                '*.config.js',
                'generate-*.js'
            ]
        }
    },
    resolve: {
        alias: {
            '@': '/src'
        }
    }
});

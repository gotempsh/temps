#!/bin/bash
set -e

# Build DMG for LocalTemps using create-dmg
# Requires: create-dmg (brew install create-dmg)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TAURI_DIR="$PROJECT_DIR/src-tauri"
DMG_DIR="$TAURI_DIR/dmg"

# App name and paths
APP_NAME="LocalTemps"
APP_BUNDLE="$TAURI_DIR/target/release/bundle/macos/${APP_NAME}.app"
DMG_OUTPUT="$PROJECT_DIR/${APP_NAME}.dmg"
BACKGROUND="$DMG_DIR/background.png"
VOLUME_ICON="$TAURI_DIR/icons/icon.icns"

# Check if app bundle exists
if [ ! -d "$APP_BUNDLE" ]; then
    echo "Error: App bundle not found at $APP_BUNDLE"
    echo "Please build the app first with: cd src-tauri && cargo tauri build"
    exit 1
fi

# Check if create-dmg is installed
if ! command -v create-dmg &> /dev/null; then
    echo "Error: create-dmg is not installed"
    echo "Install it with: brew install create-dmg"
    exit 1
fi

# Remove existing DMG if present
rm -f "$DMG_OUTPUT"

echo "Creating DMG installer..."

create-dmg \
    --volname "$APP_NAME" \
    --volicon "$VOLUME_ICON" \
    --background "$BACKGROUND" \
    --window-pos 200 120 \
    --window-size 540 380 \
    --icon-size 80 \
    --icon "$APP_NAME.app" 140 200 \
    --hide-extension "$APP_NAME.app" \
    --app-drop-link 400 200 \
    --no-internet-enable \
    "$DMG_OUTPUT" \
    "$APP_BUNDLE"

echo ""
echo "DMG created successfully: $DMG_OUTPUT"
echo ""
ls -lh "$DMG_OUTPUT"

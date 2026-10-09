#!/bin/bash
# Create a macOS .app bundle for Rift
set -e

APP_NAME="Rift"
BUNDLE_ID="com.overkazaf.rift"
VERSION="${RIFT_VERSION:-0.4.0}"
ICON_PNG="assets/icon.png"

# Build release (CI sets SKIP_BUILD=1 after producing target/release/rift itself,
# e.g. a lipo'd universal binary)
if [ -z "${SKIP_BUILD:-}" ]; then
    echo "Building release..."
    cargo build --release --features gpu,webview 2>/dev/null || cargo build --release
fi

# Create bundle structure
BUNDLE="target/${APP_NAME}.app"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS"
mkdir -p "$BUNDLE/Contents/Resources"

# Copy binary
cp "target/release/rift" "$BUNDLE/Contents/MacOS/rift"

# Generate .icns from PNG (macOS only)
if command -v sips &>/dev/null && command -v iconutil &>/dev/null; then
    ICONSET="target/rift.iconset"
    mkdir -p "$ICONSET"
    for size in 16 32 64 128 256 512; do
        sips -z $size $size "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}.png" &>/dev/null
        double=$((size * 2))
        sips -z $double $double "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" &>/dev/null
    done
    iconutil -c icns "$ICONSET" -o "$BUNDLE/Contents/Resources/rift.icns"
    rm -rf "$ICONSET"
    echo "Icon: rift.icns created"
else
    echo "Warning: sips/iconutil not found, skipping .icns generation"
fi

# Info.plist
cat > "$BUNDLE/Contents/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>rift</string>
    <key>CFBundleIconFile</key>
    <string>rift</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright 2024-2026 overkazaf. MIT License.</string>
</dict>
</plist>
PLIST

echo ""
echo "Bundle created: $BUNDLE"
echo "Run with: open $BUNDLE"
echo "Or: $BUNDLE/Contents/MacOS/rift"

#!/bin/bash
# macOS 打包：构建 release 并组装 .app bundle（含字体）。
# 用法: ./scripts/package-macos.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/Stacio.app"

echo "==> cargo build --release -p stacio-app"
cargo build --release -p stacio-app

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/fonts"
cp "$ROOT/target/release/stacio-app" "$APP/Contents/MacOS/Stacio"
cp "$ROOT/assets/fonts/JetBrainsMonoNLNerdFont-Regular.ttf" "$APP/Contents/Resources/fonts/"

cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Stacio</string>
    <key>CFBundleDisplayName</key><string>Stacio</string>
    <key>CFBundleIdentifier</key><string>com.stacio.cross</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleExecutable</key><string>Stacio</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSMultipleInstancesProhibited</key><true/>
</dict>
</plist>
EOF

# 本地测试签名（ad-hoc），无开发者证书。
codesign --force --sign - "$APP" 2>/dev/null || true

echo "==> 完成: $APP"
echo "    启动: open $APP"
echo "    冒烟: \"$APP/Contents/MacOS/Stacio\" --screenshot /tmp/stacio-smoke.png"

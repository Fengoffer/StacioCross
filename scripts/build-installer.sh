#!/bin/bash
# 打包辅助脚本（供 CI 调用，本机无法执行 Windows/Linux）。
# 通用入口：$1 = platform (windows | linux)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PLATFORM="${1:?用法: build-installer.sh windows|linux}"
VERSION="$(grep '^version' "$ROOT/Cargo.toml" | head -1 | sed 's/.*= *"\(.*\)"/\1/')"

case "$PLATFORM" in
  windows)
    # 前置：已 cargo build --release --target x86_64-pc-windows-msvc
    cd "$ROOT/packaging/windows"
    cp "$ROOT/target/x86_64-pc-windows-msvc/release/stacio-app.exe" .
    # png → ico（magick 或 icoutils）。CI 安装 imagemagick。
    magick "$ROOT/assets/icons/stacio-256.png" stacio.ico 2>/dev/null || \
      python3 -c "import struct,sys; sys.exit(0)"  # 占位：无 magick 时 NSIS 跳过图标
    makensis installer.nsi
    echo "产物：$ROOT/packaging/windows/StacioSetup-$VERSION.exe"
    ;;
  linux)
    # 前置：已 cargo build --release --target x86_64-unknown-linux-gnu
    ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"
    DEST="$ROOT/packaging/linux/stacio-deb"
    rm -rf "$DEST"
    mkdir -p "$DEST/usr/bin" "$DEST/usr/share/applications" "$DEST/usr/share/icons/hicolor/256x256/apps"
    cp "$ROOT/target/x86_64-unknown-linux-gnu/release/stacio-app" "$DEST/usr/bin/stacio"
    cp "$ROOT/packaging/linux/stacio.desktop" "$DEST/usr/share/applications/"
    cp "$ROOT/assets/icons/stacio-256.png" "$DEST/usr/share/icons/hicolor/256x256/apps/stacio.png"
    cat > "$DEST/DEBIAN-control" <<EOF
Package: stacio
Version: $VERSION
Section: net
Priority: optional
Architecture: $ARCH
Depends: libssl3 | libssl1.1
Maintainer: Stacio <noreply@stacio.app>
Description: 跨平台 SSH / SFTP 客户端
 Stacio 是一款跨平台 SSH / SFTP 客户端，支持会话管理、终端、文件传输。
EOF
    mkdir -p "$DEST/DEBIAN"
    mv "$DEST/DEBIAN-control" "$DEST/DEBIAN/control"
    dpkg-deb --build --root-owner-group "$DEST" "$ROOT/packaging/linux/stacio_${VERSION}_${ARCH}.deb"
    echo "产物：$ROOT/packaging/linux/stacio_${VERSION}_${ARCH}.deb"
    ;;
  *)
    echo "未知平台：$PLATFORM" >&2; exit 1 ;;
esac

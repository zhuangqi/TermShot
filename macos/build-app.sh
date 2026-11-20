#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET=14.0
target=${CARGO_TARGET_DIR:-target}
app="$target/release/TermShot.app"
mode=${1:-}
case "$mode" in
    '')
        cargo build --release --locked
        ;;
    --dmg)
        sdk=$(xcrun --sdk macosx --show-sdk-version)
        if [ "${sdk%%.*}" -lt 26 ]; then
            echo "The release disk image requires macOS SDK 26 or newer." >&2
            exit 1
        fi
        rustup target add aarch64-apple-darwin x86_64-apple-darwin
        cargo build --release --locked --target aarch64-apple-darwin
        cargo build --release --locked --target x86_64-apple-darwin
        ;;
    *)
        echo "Usage: $0 [--dmg]" >&2
        exit 2
        ;;
esac
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp assets/TermShot.icns "$app/Contents/Resources/TermShot.icns"
if [ "$mode" = --dmg ]; then
    xcrun lipo -create "$target/aarch64-apple-darwin/release/TermShot" \
        "$target/x86_64-apple-darwin/release/TermShot" -output "$app/Contents/MacOS/TermShot"
    xcrun lipo "$app/Contents/MacOS/TermShot" -verify_arch arm64 x86_64
else
    cp "$target/release/TermShot" "$app/Contents/MacOS/TermShot"
fi
version=$(awk -F '"' '/^version = / {print $2;exit}' Cargo.toml)
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.zhuangqi.TermShot</string>
<key>CFBundleExecutable</key><string>TermShot</string>
<key>CFBundleName</key><string>TermShot</string>
<key>CFBundleIconFile</key><string>TermShot.icns</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>LSUIElement</key><true/>
</dict></plist>
EOF
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

if [ "$mode" = --dmg ]; then
    staging=$(mktemp -d)
    trap 'rm -rf -- "$staging"' EXIT
    ditto "$app" "$staging/TermShot.app"
    ln -s /Applications "$staging/Applications"
    mkdir -p "$target/installer"
    dmg="$target/installer/TermShot-$version-macOS-Universal.dmg"
    hdiutil create -ov -volname TermShot -srcfolder "$staging" -format UDZO "$dmg"
    hdiutil verify "$dmg"
fi

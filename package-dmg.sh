#!/bin/bash
# Build the universal binary (build-universal.sh), wrap it in a minimal
# .app bundle, and produce dist/cryocon-gui-<version>-universal.dmg
# for sharing (e.g. attached to a GitHub release).
set -euo pipefail
cd "$(dirname "$0")"

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')"
APP="cryocon-gui.app"
STAGE="dist/stage"
DMG="dist/cryocon-gui-${VERSION}-universal.dmg"

./build-universal.sh

rm -rf "$STAGE"
mkdir -p "$STAGE/$APP/Contents/MacOS" "$STAGE/$APP/Contents/Resources"

cp assets/AppIcon.icns "$STAGE/$APP/Contents/Resources/AppIcon.icns"

cp dist/cryocon-gui "$STAGE/$APP/Contents/MacOS/cryocon-gui"
chmod +x "$STAGE/$APP/Contents/MacOS/cryocon-gui"
printf 'APPL????' > "$STAGE/$APP/Contents/PkgInfo"
cat > "$STAGE/$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" \
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>              <string>cryocon-gui</string>
    <key>CFBundleDisplayName</key>       <string>cryocon-gui</string>
    <key>CFBundleIdentifier</key>        <string>io.github.grebenyyk.cryocon-gui</string>
    <key>CFBundleExecutable</key>        <string>cryocon-gui</string>
    <key>CFBundleIconFile</key>          <string>AppIcon</string>
    <key>CFBundleIconName</key>          <string>AppIcon</string>
    <key>CFBundlePackageType</key>       <string>APPL</string>
    <key>CFBundleVersion</key>           <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key>    <string>11.0</string>
    <key>NSHighResolutionCapable</key>   <true/>
</dict>
</plist>
PLIST

cat > "$STAGE/README.txt" <<'NOTE'
cryocon-gui — GUI for the Cryo-con Model 22C temperature controller

First launch (the app is not signed with an Apple Developer ID), either:
  right-click cryocon-gui.app -> Open -> Open (macOS asks once),
or remove the quarantine flag in Terminal:
  xattr -dr com.apple.quarantine /Applications/cryocon-gui.app
(only for software you trust; adjust the path if it lives elsewhere)

Connect to the instrument at 192.168.1.5 port 5000 (default), or to the
offline simulator at 127.0.0.1 port 15000 (run mock_cryocon.py first).

Safety notes are in the repository README:
  https://github.com/grebenyyk/cryocon-gui
NOTE

# ad-hoc signature on the assembled bundle
codesign --force --sign - "$STAGE/$APP" >/dev/null 2>&1 || true

rm -f "$DMG"
hdiutil create -volname "cryocon-gui" -srcfolder "$STAGE" -ov -format UDZO "$DMG" \
    >/dev/null

echo "built:"
ls -lh "$DMG" | awk '{print $NF, $5}'
hdiutil imageinfo "$DMG" 2>/dev/null | grep -E "Format:|partition-name" | head -2 || true

#!/usr/bin/env bash
# Package the release `caribou` command as the nightly archive for one
# target: dist/caribou-nightly-<date>-<target>.tar.gz (a zip on Windows)
# and its .sha256. Inside: the command, ash's runtime image beside it
# under the names an HDLL imports (as ash's own release ships it: the
# command links the runtime in, and the sibling copy is for programs with
# HDLLs beside them), the haxelib, the docs and the license.
#
# Usage: scripts/package_nightly.sh <target-triple> [<yyyymmdd>]
# Expects target/release/caribou and ../ash/target/release/<runtime>.
set -euo pipefail

TARGET="${1:?usage: package_nightly.sh <target-triple> [<yyyymmdd>]}"
STAMP="${2:-$(date -u +%Y%m%d)}"

case "$(uname -s)" in
  Darwin) EXE=caribou; STD=libash_std.dylib; HL=libhl.dylib; ARCHIVE=tar ;;
  MINGW*|MSYS*|CYGWIN*) EXE=caribou.exe; STD=ash_std.dll; HL=libhl.dll; ARCHIVE=zip ;;
  *) EXE=caribou; STD=libash_std.so; HL=""; ARCHIVE=tar ;;
esac

BIN="target/release/$EXE"
test -f "$BIN" || { echo "error: $BIN not built" >&2; exit 1; }

NAME="caribou-nightly-${STAMP}-${TARGET}"
DIST="dist/$NAME"
rm -rf "$DIST"
mkdir -p "$DIST"
cp "$BIN" "$DIST/"
cp -R haxe "$DIST/haxe"
cp -R docs "$DIST/docs"
cp README.md LICENSE "$DIST/"

STD_SRC="../ash/target/release/$STD"
if [[ -f "$STD_SRC" ]]; then
  cp "$STD_SRC" "$DIST/$STD"
  chmod u+w "$DIST/$STD"
  if [[ -n "$HL" ]]; then
    # A program's HDLLs import this name; ash then runs its own std through
    # the same image, so there is one GC.
    cp "$STD_SRC" "$DIST/$HL"
    chmod u+w "$DIST/$HL"
  fi
  case "$(uname -s)" in
    Darwin)
      install_name_tool -id "@executable_path/$HL" "$DIST/$HL"
      # HashLink 1.x builds import libhl.1.dylib: the same image.
      ln -s "$HL" "$DIST/libhl.1.dylib"
      # Rewriting a load command invalidates the signature; unsigned, dlopen
      # registers one in the kernel on first open, which stalls.
      codesign --force -s - "$DIST/$STD"
      codesign --force -s - "$DIST/$HL"
      codesign --force -s - "$DIST/$EXE"
      ;;
    MINGW*|MSYS*|CYGWIN*)
      # A copy, not a link: a Windows symlink needs Developer Mode.
      cp "$STD_SRC" "$DIST/libhl.1.dll"
      ;;
  esac
  echo "bundled runtime: $STD_SRC"
else
  echo "warning: $STD_SRC not found; the archive carries the command alone" >&2
fi

mkdir -p dist
case "$ARCHIVE" in
  tar)
    tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
    FILE="$NAME.tar.gz"
    ;;
  zip)
    (cd dist && 7z a -tzip -bso0 -bsp0 "$NAME.zip" "$NAME")
    FILE="$NAME.zip"
    ;;
esac
if command -v sha256sum >/dev/null; then
  (cd dist && sha256sum "$FILE" > "$FILE.sha256")
else
  (cd dist && shasum -a 256 "$FILE" > "$FILE.sha256")
fi
echo "wrote dist/$FILE"

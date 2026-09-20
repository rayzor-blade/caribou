#!/usr/bin/env bash
# Package the release `caribou` command as the nightly archive for one
# target: dist/caribou-nightly-<target>.tar.gz (a zip on Windows) and its
# .sha256. The name carries no date, so the installer's URL is stable;
# the release's title and notes say which night it is. Inside: the command, ash's runtime image beside it
# under the names an HDLL imports (as ash's own release ships it: the
# command links the runtime in, and the sibling copy is for programs with
# HDLLs beside them), the haxelib, the docs and the license.
#
# The command links LLVM statically. On macOS the few Homebrew dylibs it
# still references (z3 and what that pulls in) live at paths only a
# machine with Homebrew has, so they are copied beside it with their load
# commands rewritten to @executable_path. On Windows the DLLs LLVM imports
# (zlib, zstd, libxml2) ride beside it too, from target/release, where the
# workflow put them. On Linux the equivalents are distro packages and
# stay dynamic.
#
# Usage: scripts/package_nightly.sh <target-triple>
# Expects target/release/caribou and ../ash/target/release/<runtime>.
set -euo pipefail

TARGET="${1:?usage: package_nightly.sh <target-triple>}"

case "$(uname -s)" in
  Darwin) EXE=caribou; STD=libash_std.dylib; HL=libhl.dylib; ARCHIVE=tar ;;
  MINGW*|MSYS*|CYGWIN*) EXE=caribou.exe; STD=ash_std.dll; HL=libhl.dll; ARCHIVE=zip ;;
  *) EXE=caribou; STD=libash_std.so; HL=""; ARCHIVE=tar ;;
esac

BIN="target/release/$EXE"
test -f "$BIN" || { echo "error: $BIN not built" >&2; exit 1; }

NAME="caribou-nightly-${TARGET}"
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

case "$(uname -s)" in
  Darwin)
    # Every non-system dylib the command or the runtime image references,
    # and what those reference in turn, brought beside them.
    machos=("$DIST/$EXE")
    [[ -f "$DIST/$STD" ]] && machos+=("$DIST/$STD" "$DIST/$HL")
    for macho in "${machos[@]}"; do
      otool -L "$macho" | awk 'NR>1 {print $1}' | while read -r dep; do
        case "$dep" in
          /usr/lib/*|/System/*|@*) continue ;;
        esac
        name="$(basename "$dep")"
        [[ -f "$DIST/$name" ]] || { cp "$dep" "$DIST/$name"; chmod u+w "$DIST/$name"; }
        install_name_tool -change "$dep" "@executable_path/$name" "$macho"
        install_name_tool -id "@executable_path/$name" "$DIST/$name"
        otool -L "$DIST/$name" | awk 'NR>1 {print $1}' | while read -r sub; do
          case "$sub" in
            /usr/lib/*|/System/*|@*) continue ;;
          esac
          subname="$(basename "$sub")"
          [[ -f "$DIST/$subname" ]] || { cp "$sub" "$DIST/$subname"; chmod u+w "$DIST/$subname"; }
          install_name_tool -change "$sub" "@executable_path/$subname" "$DIST/$name"
        done
        codesign --force -s - "$DIST/$name"
      done
    done
    # Signing last: rewriting a load command invalidates the signature, and
    # unsigned, dlopen registers one in the kernel on first open, which
    # stalls.
    for macho in "${machos[@]}"; do
      codesign --force -s - "$macho"
    done
    echo "dynamic dependencies:"
    otool -L "$DIST/$EXE" | sed -n '2,20p'
    ;;
  MINGW*|MSYS*|CYGWIN*)
    for dll in zlib.dll zstd.dll libxml2.dll; do
      test -f "target/release/$dll" || { echo "error: target/release/$dll is not there; LLVM imports it" >&2; exit 1; }
      cp "target/release/$dll" "$DIST/"
    done
    ;;
  *)
    echo "dynamic dependencies (from distro packages):"
    ldd "$DIST/$EXE" | grep -v 'linux-vdso\|ld-linux\|libc\.\|libm\.\|libgcc\|libpthread\|libdl' || true
    ;;
esac

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

#!/bin/sh
# Caribou installer — https://github.com/rayzor-blade/caribou
#
#   curl -fsSL https://caribou.rayzor.tech/install.sh | sh
#
# Downloads the nightly `caribou` command for this platform, verifies its
# SHA-256, unpacks it into ~/.caribou/bin (the command, ash's runtime image
# beside it, the haxelib and the docs) and adds that directory to your PATH.
#
#   CARIBOU_INSTALL_DIR   where to install; default ~/.caribou/bin
set -eu

REPO="rayzor-blade/caribou"
DEST="${CARIBOU_INSTALL_DIR:-$HOME/.caribou/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Darwin/arm64)  target="aarch64-apple-darwin" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Linux/x86_64)  target="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) echo "error: no prebuilt caribou for $os/$arch; see https://github.com/$REPO/blob/main/docs/building.md" >&2; exit 1 ;;
esac

for tool in curl tar; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 1; }
done

name="caribou-nightly-${target}"
asset="${name}.tar.gz"
url="https://github.com/${REPO}/releases/download/nightly/${asset}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading ${asset} ..."
curl -fsSL -o "$tmp/$asset" "$url" || {
  echo "error: no nightly archive for ${target} at $url" >&2
  echo "  the last night's build may have failed on this platform; see https://github.com/$REPO/releases/tag/nightly" >&2
  exit 1
}
curl -fsSL -o "$tmp/$asset.sha256" "$url.sha256" || {
  echo "error: no checksum beside the archive" >&2
  exit 1
}

# The checksum file names the archive as it was written; only the hash
# is compared.
expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
else
  echo "error: neither sha256sum nor shasum is available to verify the download" >&2
  exit 1
fi
[ "$actual" = "$expected" ] || {
  echo "error: checksum mismatch for $asset (got $actual, expected $expected)" >&2
  exit 1
}

# The archive holds one directory; its contents go into DEST as they
# are, so the runtime image and the dylibs stay beside the command.
mkdir -p "$DEST"
tar xzf "$tmp/$asset" -C "$DEST" --strip-components=1
chmod +x "$DEST/caribou"

# A quick sanity run; a missing shared library shows up here, not later.
if ! "$DEST/caribou" 2>&1 | grep -q 'usage:'; then
  echo "warning: $DEST/caribou did not run cleanly." >&2
  if [ "$os" = "Linux" ]; then
    echo "  missing shared libraries? try:" >&2
    echo "  sudo apt-get install -y libzstd1 zlib1g libxml2 libtinfo6 libedit2 libffi8" >&2
  fi
fi

# A shell-agnostic env file, so there is always exactly one thing to
# source regardless of which rc the user's shell actually reads.
env_file="$(dirname "$DEST")/env"
cat > "$env_file" <<EOF
# Added by the caribou installer. Source this from your shell config.
case ":\${PATH}:" in
  *":$DEST:"*) ;;
  *) export PATH="$DEST:\$PATH" ;;
esac
EOF

line=". \"$env_file\""

# Which rc this user's shell actually reads, from \$SHELL rather than
# the first rc file that happens to exist.
case "$(basename "${SHELL:-sh}")" in
  zsh)  rcs="$HOME/.zshrc" ;;
  bash) rcs="$HOME/.bashrc" ;;
  fish) rcs="" ;;
  *)    rcs="$HOME/.profile" ;;
esac

added=""
for rc in $rcs; do
  [ -e "$rc" ] || : > "$rc"
  if ! grep -qs "$env_file" "$rc"; then
    printf '\n# Added by the caribou installer\n%s\n' "$line" >> "$rc"
  fi
  added="$rc"
  break
done

echo
echo "installed: $DEST/caribou"
echo "haxelib:   haxelib dev caribou $DEST/haxe"

case ":${PATH}:" in
  *":$DEST:"*)
    echo
    echo "caribou is on your PATH. Try:  caribou run bin/main.hl"
    exit 0
    ;;
esac

# This script runs in its own shell and cannot change the PATH of the
# shell that invoked it, which is why the next line matters.
echo
echo "----------------------------------------------------------------"
if [ -n "$added" ]; then
  echo "PATH updated in $added, but NOT in this shell."
  echo "Run this now (or open a new terminal):"
else
  echo "Add caribou to your PATH: run this now, and add it to your shell config:"
fi
echo
echo "    $line"
echo
echo "then:  caribou run bin/main.hl"
echo "----------------------------------------------------------------"

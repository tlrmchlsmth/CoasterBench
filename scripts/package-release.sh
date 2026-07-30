#!/usr/bin/env bash
# Packages a relocatable coasterbench-cli tarball: the binary, the OpenRCT2 data
# it needs at runtime, and every non-system shared library it links against.
# No RollerCoaster Tycoon 2 data is included; the user supplies that.
#
#   ./scripts/package-release.sh [build-dir] [output-dir]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${1:-$ROOT/build}"
OUT="${2:-$ROOT/dist}"
BIN="$BUILD/coasterbench-cli"

[ -x "$BIN" ] || { echo "no binary at $BIN; build the openrct2-cli target first" >&2; exit 1; }

harness_version() { grep -o '"[^"]*"' "$ROOT/src/openrct2/rustbridge/HarnessVersion.h" | tr -d '"'; }
upstream_version() { grep 'kOpenRCT2Version' "$ROOT/src/openrct2/Version.h" | grep -o '"[^"]*"' | tr -d '"'; }

HV="$(harness_version)"
UV="$(upstream_version)"
case "$(uname -s)" in
    Darwin) OS=macos ;;
    Linux)  OS=linux ;;
    *)      echo "unsupported platform: $(uname -s)" >&2; exit 1 ;;
esac
ARCH="$(uname -m)"
NAME="coasterbench-cli-$HV-openrct2-$UV-$OS-$ARCH"
STAGE="$OUT/$NAME"

rm -rf "$STAGE"
mkdir -p "$STAGE/data" "$STAGE/lib"
cp "$BIN" "$STAGE/"

# Runtime data. The .dat files come from the `graphics` target; the object,
# sequence and asset packs are the same zips cmake fetches, taken straight from
# assets.json so a CLI-only build does not have to have run the GUI's rules.
cp -R "$ROOT/data/language" "$ROOT/data/scenario_patches" "$ROOT/data/shaders" "$STAGE/data/"
for dat in g2 fonts palettes tracks; do
    [ -f "$BUILD/$dat.dat" ] || { echo "missing $BUILD/$dat.dat; build the graphics target" >&2; exit 1; }
    cp "$BUILD/$dat.dat" "$STAGE/data/"
done

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

fetch_assets() {
    local key="$1" dest="$2" url expected tmp
    url=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]]['url'])" "$ROOT/assets.json" "$key")
    expected=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]]['sha256'])" "$ROOT/assets.json" "$key")
    tmp="$(mktemp -d)"
    curl -sSL "$url" -o "$tmp/asset.zip"
    [ "$(sha256 "$tmp/asset.zip")" = "$expected" ] || { echo "checksum mismatch for $key" >&2; exit 1; }
    mkdir -p "$dest"
    unzip -qo "$tmp/asset.zip" -d "$dest"
    rm -rf "$tmp"
}

fetch_assets objects "$STAGE/data/object"
fetch_assets title-sequences "$STAGE/data/sequence"
fetch_assets opensfx "$STAGE/data"
fetch_assets openmusic "$STAGE/data"

# Shared libraries. Anything outside the system prefixes travels with us, and
# the binary is retargeted at the bundled copies so the tarball runs anywhere.
if [ "$OS" = macos ]; then
    collect() {
        local target="$1" dep base
        while read -r dep; do
            case "$dep" in
                @rpath/*) base="${dep#@rpath/}"; dep="$ROOT/lib/macos/lib/$base" ;;
                /usr/lib/*|/System/*) continue ;;
                *) base="$(basename "$dep")" ;;
            esac
            [ -f "$dep" ] || continue
            [ -f "$STAGE/lib/$base" ] && continue
            cp "$dep" "$STAGE/lib/$base"
            collect "$dep"
        done < <(otool -L "$target" | tail -n +2 | awk '{print $1}')
    }
    collect "$BIN"
    install_name_tool -add_rpath "@loader_path/lib" "$STAGE/coasterbench-cli"
else
    while read -r base path; do
        case "$path" in
            /lib/*|/lib64/*|/usr/lib/*|/usr/lib64/*)
                # glibc and its siblings must match the running kernel's loader;
                # bundling them breaks more than it fixes.
                case "$base" in
                    libc.so.*|libm.so.*|libpthread.so.*|libdl.so.*|librt.so.*|ld-linux*|libgcc_s.so.*|libstdc++.so.*) continue ;;
                esac
                cp -n "$path" "$STAGE/lib/$base" || true
                ;;
        esac
    done < <(ldd "$BIN" | awk '/=>/ {print $1, $3}')
    patchelf --set-rpath '$ORIGIN/lib' "$STAGE/coasterbench-cli"
fi

cp "$ROOT/licence.txt" "$ROOT/readme.md" "$STAGE/"
mkdir -p "$STAGE/docs"
cp "$ROOT/docs/running-the-eval.md" "$STAGE/docs/"

cat > "$STAGE/NOTICE" <<EOF
CoasterBench $HV, built on OpenRCT2 $UV.

This is a MODIFIED version of OpenRCT2 (https://github.com/OpenRCT2/OpenRCT2),
not an official OpenRCT2 release. Report issues against
https://github.com/wseaton/CoasterBench, never to the OpenRCT2 project.

Licensed under the GNU General Public License version 3 or later; see
licence.txt. Complete corresponding source for this binary:
https://github.com/wseaton/CoasterBench at tag v$HV+openrct2-$UV

data/ contains OpenRCT2's own assets (objects, title sequences, sounds, music)
under their upstream licences. It contains NO RollerCoaster Tycoon 2 data: a
legitimate copy of the original game is required to run anything, as described
in docs/running-the-eval.md.
EOF

tar -czf "$OUT/$NAME.tar.gz" -C "$OUT" "$NAME"
echo "$OUT/$NAME.tar.gz"

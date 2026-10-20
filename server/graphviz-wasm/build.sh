#!/usr/bin/env bash
#
# Build graphviz.wasm: the Graphviz `dot` command compiled to a WASI
# preview 1 command module (wasm32-wasip1), with expat for HTML-like labels.
#
# Every input is pinned by version and verified by SHA-256 before use.
#
# Usage:
#   ./build.sh                 build into this directory
#
# Environment (all optional):
#   WASI_SDK_PATH       use an existing wasi-sdk install instead of downloading
#                       one (it must be version $WASI_SDK_VERSION)
#   GRAPHVIZ_WASM_WORK  scratch directory for downloads and build trees
#                       (default: ./.work next to this script)
#   GRAPHVIZ_WASM_OUT   directory that receives graphviz.wasm and
#                       graphviz.wasm.sha256 (default: this directory)
#   JOBS                parallel make jobs (default: 2)

set -euo pipefail

# ---------------------------------------------------------------------------
# Pinned inputs
# ---------------------------------------------------------------------------

GRAPHVIZ_VERSION=16.1.0
GRAPHVIZ_URL="https://gitlab.com/api/v4/projects/4207231/packages/generic/graphviz-releases/${GRAPHVIZ_VERSION}/graphviz-${GRAPHVIZ_VERSION}.tar.xz"
GRAPHVIZ_SHA256=0f661718f3e5268dc3bad0fb53ab646605d26db70f4080799aff1dc2f61783c3

EXPAT_VERSION=2.9.0
EXPAT_URL="https://github.com/libexpat/libexpat/releases/download/R_${EXPAT_VERSION//./_}/expat-${EXPAT_VERSION}.tar.xz"
EXPAT_SHA256=1e6371862cc31999b368c3b89b49994f0677e1bab5f1b2b85ae3741f5d803051

WASI_SDK_VERSION=34.0
WASI_SDK_TAG=wasi-sdk-34
WASI_SDK_SHA256_x86_64_linux=b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4
WASI_SDK_SHA256_arm64_linux=f7e243dff54d60bcc576e94d6166b69f410f2500ae4a9ceef34315be10e77971
WASI_SDK_SHA256_arm64_macos=9c59398106b417f8f14913380fdf0097a8cc0ff4af9eb3ce0065a859e88d49e9
WASI_SDK_SHA256_x86_64_macos=87d27fa8adc68dee59bfbf2e22a6d34ef717c34d6bf1d8af2a56fc929d9ce0eb

# ---------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${GRAPHVIZ_WASM_WORK:-$HERE/.work}"
OUT="${GRAPHVIZ_WASM_OUT:-$HERE}"
JOBS="${JOBS:-2}"

export LC_ALL=C TZ=UTC
# Keep host pkg-config files (pango, cairo, gd, ...) out of the cross build.
export PKG_CONFIG_PATH=
export SOURCE_DATE_EPOCH=0

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# fetch URL SHA256 DEST: download once, verify every time.
fetch() {
  local url="$1" sha="$2" dest="$3"
  if [ ! -f "$dest" ]; then
    log "downloading $(basename "$dest")"
    curl -fsSL --retry 3 -o "$dest.part" "$url"
    mv "$dest.part" "$dest"
  fi
  local got
  got="$(sha256_of "$dest")"
  if [ "$got" != "$sha" ]; then
    rm -f "$dest"
    die "checksum mismatch for $(basename "$dest"): expected $sha, got $got"
  fi
}

for tool in curl tar patch make cmake; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done

mkdir -p "$WORK/downloads" "$OUT"
DL="$WORK/downloads"

# --- wasi-sdk ---------------------------------------------------------------

if [ -z "${WASI_SDK_PATH:-}" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)            host=x86_64-linux;  sha=$WASI_SDK_SHA256_x86_64_linux ;;
    Linux-aarch64|Linux-arm64) host=arm64-linux; sha=$WASI_SDK_SHA256_arm64_linux ;;
    Darwin-arm64)            host=arm64-macos;   sha=$WASI_SDK_SHA256_arm64_macos ;;
    Darwin-x86_64)           host=x86_64-macos;  sha=$WASI_SDK_SHA256_x86_64_macos ;;
    *) die "no pinned wasi-sdk for $(uname -s)-$(uname -m); set WASI_SDK_PATH" ;;
  esac
  name="wasi-sdk-${WASI_SDK_VERSION}-${host}"
  fetch "https://github.com/WebAssembly/wasi-sdk/releases/download/${WASI_SDK_TAG}/${name}.tar.gz" \
    "$sha" "$DL/${name}.tar.gz"
  WASI_SDK_PATH="$WORK/$name"
  if [ ! -x "$WASI_SDK_PATH/bin/clang" ]; then
    log "extracting $name"
    rm -rf "$WASI_SDK_PATH"
    tar -xzf "$DL/${name}.tar.gz" -C "$WORK"
  fi
fi
[ -x "$WASI_SDK_PATH/bin/clang" ] || die "no clang in WASI_SDK_PATH=$WASI_SDK_PATH"
sdk_version="$(head -n1 "$WASI_SDK_PATH/VERSION" 2>/dev/null || true)"
[ "$sdk_version" = "$WASI_SDK_VERSION" ] ||
  die "wasi-sdk at $WASI_SDK_PATH is version '${sdk_version}', expected $WASI_SDK_VERSION"

SYSROOT="$WASI_SDK_PATH/share/wasi-sysroot"
TARGET=wasm32-wasip1

# --- sources ----------------------------------------------------------------

fetch "$GRAPHVIZ_URL" "$GRAPHVIZ_SHA256" "$DL/graphviz-${GRAPHVIZ_VERSION}.tar.xz"
fetch "$EXPAT_URL" "$EXPAT_SHA256" "$DL/expat-${EXPAT_VERSION}.tar.xz"

# Always start from pristine trees so a rebuild never picks up stale state.
SRC="$WORK/src"
BUILD="$WORK/build"
PREFIX="$WORK/prefix"
rm -rf "$SRC" "$BUILD" "$PREFIX"
mkdir -p "$SRC" "$BUILD" "$PREFIX"

log "extracting sources"
tar -xJf "$DL/graphviz-${GRAPHVIZ_VERSION}.tar.xz" -C "$SRC"
tar -xJf "$DL/expat-${EXPAT_VERSION}.tar.xz" -C "$SRC"
GV_SRC="$SRC/graphviz-${GRAPHVIZ_VERSION}"
EXPAT_SRC="$SRC/expat-${EXPAT_VERSION}"

for p in "$HERE"/patches/*.patch; do
  [ -e "$p" ] || continue
  log "applying $(basename "$p")"
  patch -d "$GV_SRC" -p1 --forward --batch --quiet < "$p"
done

# Flags shared by every C compilation. The prefix maps keep absolute build
# paths (used by assert() messages) out of the output, so the module does not
# depend on where it was built.
COMMON_CFLAGS="-Oz -ffile-prefix-map=$SRC/= -ffile-prefix-map=$BUILD/= \
-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_GETPID"

# --- expat ------------------------------------------------------------------

log "building expat ${EXPAT_VERSION}"
cmake -S "$EXPAT_SRC" -B "$BUILD/expat" -G "Unix Makefiles" \
  -DCMAKE_TOOLCHAIN_FILE="$WASI_SDK_PATH/share/cmake/wasi-sdk-p1.cmake" \
  -DWASI_SDK_PREFIX="$WASI_SDK_PATH" \
  -DCMAKE_BUILD_TYPE=MinSizeRel \
  -DCMAKE_C_FLAGS_MINSIZEREL="$COMMON_CFLAGS -DNDEBUG" \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DEXPAT_SHARED_LIBS=OFF \
  -DEXPAT_BUILD_TOOLS=OFF -DEXPAT_BUILD_EXAMPLES=OFF \
  -DEXPAT_BUILD_TESTS=OFF -DEXPAT_BUILD_DOCS=OFF \
  -DEXPAT_BUILD_PKGCONFIG=ON >"$BUILD/expat-cmake.log"
cmake --build "$BUILD/expat" -j "$JOBS" >"$BUILD/expat-build.log"
cmake --install "$BUILD/expat" >/dev/null

# --- Graphviz ---------------------------------------------------------------

log "configuring graphviz ${GRAPHVIZ_VERSION}"
CC="$WASI_SDK_PATH/bin/clang --target=$TARGET --sysroot=$SYSROOT"
CXX="$WASI_SDK_PATH/bin/clang++ --target=$TARGET --sysroot=$SYSROOT"
GV_CFLAGS="$COMMON_CFLAGS"
GV_CXXFLAGS="-Oz -fno-exceptions -ffile-prefix-map=$SRC/= -ffile-prefix-map=$BUILD/= -D_WASI_EMULATED_SIGNAL"
# wasm-ld reserves only 64 KiB for the C stack by default. Graphviz recurses
# deeply on large graphs, and an overflow of that stack is a memory fault in
# the module, so give it 8 MiB, the default thread stack of native `dot`.
GV_LDFLAGS="-L$PREFIX/lib -Wl,-z,stack-size=8388608"
GV_LIBS="-lwasi-emulated-signal -lwasi-emulated-process-clocks -lwasi-emulated-getpid"

mkdir -p "$BUILD/graphviz"
(
  cd "$BUILD/graphviz"
  PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig" \
  CC="$CC" CXX="$CXX" \
  AR="$WASI_SDK_PATH/bin/llvm-ar" RANLIB="$WASI_SDK_PATH/bin/llvm-ranlib" \
  NM="$WASI_SDK_PATH/bin/llvm-nm" \
  CFLAGS="$GV_CFLAGS" CXXFLAGS="$GV_CXXFLAGS" \
  CPPFLAGS="-I$PREFIX/include" LDFLAGS="$GV_LDFLAGS" LIBS="$GV_LIBS" \
  "$GV_SRC/configure" \
    --host=wasm32-unknown-wasi \
    --prefix=/usr \
    --disable-shared --enable-static --disable-ltdl --without-x \
    --disable-swig --disable-sharp --disable-go --disable-guile \
    --disable-java --disable-lua --disable-perl --disable-php \
    --disable-python --disable-python3 --disable-r --disable-ruby \
    --disable-tcl --disable-man-pdfs \
    --with-expat --with-sfdp --with-ortho --with-digcola \
    --without-ipsepcola --without-gts --without-ann \
    --without-devil --without-webp --without-poppler --without-rsvg \
    --without-ghostscript --without-pangocairo --without-lasi \
    --without-freetype2 --without-gdk --without-gdk-pixbuf --without-gtk \
    --without-gtkgl --without-gtkglext --without-glade --without-qt \
    --without-quartz --without-gdiplus --without-libgd --without-glut \
    --without-smyrna --without-aalib \
    >"$BUILD/graphviz-configure.log" 2>&1 ||
    { tail -n 40 "$BUILD/graphviz-configure.log" >&2; die "graphviz configure failed"; }
)
grep -q '^#define HAVE_EXPAT 1' "$BUILD/graphviz/config.h" ||
  die "configure did not find expat; HTML-like labels would not work"

# configure appends host-specific flags (for example a Mach-O linker option
# when building on macOS), so the exact flags are passed again to make.
MAKE_FLAGS=(CFLAGS="$GV_CFLAGS" CXXFLAGS="$GV_CXXFLAGS" LDFLAGS="$GV_LDFLAGS")

# Only the libraries the dot command links are built; tools such as gvpr
# are not needed.
GV_DIRS=(
  lib/util lib/cdt lib/xdot lib/cgraph lib/pathplan lib/rbtree lib/ortho
  lib/sparse lib/patchwork lib/common lib/pack lib/label lib/gvc
  lib/circogen lib/dotgen lib/fdpgen lib/sfdpgen lib/neatogen lib/twopigen
  lib/osage
  plugin/core plugin/dot_layout plugin/neato_layout plugin/kitty plugin/vt
)
for d in "${GV_DIRS[@]}"; do
  log "building $d"
  make -C "$BUILD/graphviz/$d" -j "$JOBS" "${MAKE_FLAGS[@]}" \
    >"$BUILD/make-${d//\//_}.log" 2>&1 ||
    { tail -n 40 "$BUILD/make-${d//\//_}.log" >&2; die "build of $d failed"; }
done
log "linking dot_static"
make -C "$BUILD/graphviz/cmd/dot" "${MAKE_FLAGS[@]}" dot_static \
  >"$BUILD/make-cmd_dot.log" 2>&1 ||
  { tail -n 40 "$BUILD/make-cmd_dot.log" >&2; die "link of dot_static failed"; }

# --- output -----------------------------------------------------------------

# Drop DWARF but keep the name section, so traps still show function names.
"$WASI_SDK_PATH/bin/llvm-objcopy" --strip-debug \
  "$BUILD/graphviz/cmd/dot/dot_static" "$OUT/graphviz.wasm"
(cd "$OUT" && printf '%s  graphviz.wasm\n' "$(sha256_of graphviz.wasm)" > graphviz.wasm.sha256)

log "wrote $OUT/graphviz.wasm ($(wc -c < "$OUT/graphviz.wasm" | tr -d ' ') bytes)"
cat "$OUT/graphviz.wasm.sha256"

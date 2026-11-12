#!/usr/bin/env bash
set -euo pipefail

architecture=${1:-aarch64}
case "$architecture" in
  aarch64|x86_64) ;;
  *) echo "Expected aarch64 or x86_64" >&2; exit 2 ;;
esac

repository=$(cd "$(dirname "$0")/../.." && pwd)
build="$repository/target/torrent-windows-$architecture-cross"
mkdir -p "$build"

docker build --platform linux/arm64 -t hydra-torrent-windows-builder \
  -f "$repository/scripts/plugins/torrent-windows.Dockerfile" "$repository/scripts/plugins"
docker run --rm --platform linux/arm64 -e HYDRA_WINDOWS_ARCH="$architecture" -v "$repository:/source:ro" -v "$build:/build" hydra-torrent-windows-builder bash -euc '
  exec 9>/build/build.lock
  flock 9
  cd /build
  compiler_prefix="$HYDRA_WINDOWS_ARCH-w64-mingw32"
  if [ "$HYDRA_WINDOWS_ARCH" = aarch64 ]; then
    openssl_target=mingwarm64
    processor=ARM64
  else
    openssl_target=mingw64
    processor=AMD64
  fi
  if [ ! -x "toolchain/bin/$compiler_prefix-g++" ]; then
    curl -fL --retry 3 -o toolchain.tar.xz https://github.com/mstorsjo/llvm-mingw/releases/download/20260922/llvm-mingw-20260922-ucrt-ubuntu-22.04-aarch64.tar.xz
    echo "07d21263c56bfe9a713db6fdb3f7434bf4c121a005e40397d3b4c0170fb06769  toolchain.tar.xz" | sha256sum -c -
    mkdir -p toolchain
    tar -xf toolchain.tar.xz -C toolchain --strip-components=1
  fi
  export PATH=/build/toolchain/bin:$PATH
  mkdir -p headers json-cmake
  cp -a /usr/include/boost /usr/include/nlohmann headers/
  cp /usr/share/cmake/nlohmann_json/nlohmann_jsonConfigVersion.cmake json-cmake/
  cat > json-cmake/nlohmann_jsonConfig.cmake <<EOF
add_library(nlohmann_json::nlohmann_json INTERFACE IMPORTED)
set_target_properties(nlohmann_json::nlohmann_json PROPERTIES INTERFACE_INCLUDE_DIRECTORIES /build/headers)
EOF
  if [ ! -f openssl-install/lib/libssl.a ]; then
    curl -fL --retry 3 -o openssl.tar.gz https://github.com/openssl/openssl/releases/download/openssl-3.6.5/openssl-3.6.5.tar.gz
    echo "a2157c2830efdec3788939b00c9b0638306d3f0bbb76dc4832ee503bb397df98  openssl.tar.gz" | sha256sum -c -
    tar -xf openssl.tar.gz
    cd openssl-3.6.5
    perl Configure "$openssl_target" --cross-compile-prefix="$compiler_prefix-" no-shared no-tests no-apps no-asm --prefix=/build/openssl-install --libdir=lib
    make -j4
    make install_dev
    cd /build
  fi
  cat > toolchain.cmake <<EOF
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR $processor)
set(CMAKE_C_COMPILER /build/toolchain/bin/$compiler_prefix-gcc)
set(CMAKE_CXX_COMPILER /build/toolchain/bin/$compiler_prefix-g++)
set(CMAKE_RC_COMPILER /build/toolchain/bin/$compiler_prefix-windres)
set(CMAKE_SHARED_LINKER_FLAGS "-static -Wl,--pdb=/build/native/hydra-torrent-native.pdb")
EOF
  cmake -S /source/plugins/hydra-torrent/native -B /build/native \
    -DCMAKE_TOOLCHAIN_FILE=/build/toolchain.cmake -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_CXX_FLAGS="-g -gcodeview" -DOPENSSL_ROOT_DIR=/build/openssl-install \
    -DBoost_NO_BOOST_CMAKE=ON -DBoost_INCLUDE_DIR=/build/headers -Dnlohmann_json_DIR=/build/json-cmake \
    -DHYDRA_STATIC_ENGINE=ON
  # libc++ 23 no longer provides <iterator> through another standard header.
  source=/build/native/_deps/libtorrent-src/src/escape_string.cpp
  if ! grep -q "^#include <iterator>" "$source"; then
    sed -i "/#include <string>/a #include <iterator>" "$source"
  fi
  cmake --build /build/native --parallel 4
  cat /build/toolchain/LICENSE.TXT \
    /build/toolchain/$compiler_prefix/share/mingw32/COPYING.MinGW-w64-runtime.txt \
    /build/toolchain/$compiler_prefix/share/mingw32/COPYING.winpthreads.txt > /build/native/LICENSE-extra
'

python3 "$repository/plugins/hydra-torrent/build.py" --skip-native-build \
  --platform "windows-$architecture" --native-build "$build/native" \
  --output "$build/$(python3 "$repository/scripts/plugins/plugin_release.py" name hydra-torrent --platform "windows-$architecture")"

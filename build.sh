#!/usr/bin/env bash
# sv-rs 静态发布构建。
#
# 用 cargo-zigbuild + zig 产出 Linux musl 全静态二进制(amd64 / arm64),
# 复制到 dist/ 并附 SHA-256 校验。产物不依赖 glibc,可直接拷到任何 Linux 主机。
#
# 依赖:rustup、cargo-zigbuild、zig。
#    cargo install --locked cargo-zigbuild
#    zig 下载: https://ziglang.org/download/
#
# 用法:
#   ./build.sh                 # 构建全部受支持目标
#   ./build.sh <triple> ...    # 仅构建指定目标
set -euo pipefail
cd "$(dirname "$0")"

CRATE="sv-rs"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
[ -n "$VERSION" ] || { echo "无法从 Cargo.toml 读取版本" >&2; exit 1; }

DIST="dist"
ALL_TARGETS=("x86_64-unknown-linux-musl" "aarch64-unknown-linux-musl")
if (($# > 0)); then
  TARGETS=("$@")
else
  TARGETS=("${ALL_TARGETS[@]}")
fi

# 每个 triple 对应的产物命名(与 CI 发布资产保持一致)。
triple_arch() {
  case "$1" in
    x86_64-unknown-linux-musl) echo "amd64" ;;
    aarch64-unknown-linux-musl) echo "arm64" ;;
    *) echo "" ;;
  esac
}

command -v cargo >/dev/null || { echo "缺少 cargo(请先安装 rustup)" >&2; exit 1; }
command -v cargo-zigbuild >/dev/null || { echo "缺少 cargo-zigbuild (cargo install --locked cargo-zigbuild)" >&2; exit 1; }
command -v zig >/dev/null || { echo "缺少 zig(下载: https://ziglang.org/download/)" >&2; exit 1; }

mkdir -p "$DIST"
SUMS="$DIST/SHA256SUMS"
: > "$SUMS"

for triple in "${TARGETS[@]}"; do
  arch="$(triple_arch "$triple")"
  [ -n "$arch" ] || { echo "不支持的目标: $triple" >&2; exit 1; }

  echo "==> [$triple] 添加 rust target"
  rustup target add "$triple" >/dev/null

  echo "==> [$triple] cargo zigbuild --release (静态 musl, v$VERSION)"
  cargo zigbuild --release --target "$triple"

  name="$CRATE-linux-$arch"
  bin="target/$triple/release/$CRATE"
  install -m 0755 "$bin" "$DIST/$name"
  (cd "$DIST" && sha256sum "$name") >> "$SUMS"
  echo "    -> $DIST/$name"
done

echo
echo "==> 构建完成,校验和: $SUMS"
cat "$SUMS"

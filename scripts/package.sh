#!/usr/bin/env bash
# LaterScreen 一键打包：构建目标平台的 release 二进制并归档到 dist/。
#
# 用法：
#   scripts/package.sh                 # 打包默认目标集中本机具备工具链的目标
#   scripts/package.sh <target>...     # 只打包指定目标（rustc 三元组）
#   scripts/package.sh --list          # 列出默认目标集与本机可用性
#
# 依赖说明：
#   - 项目链接期仅依赖 libc（X11/GL 运行时 dlopen），Linux 交叉编译
#     只需对应的 gcc 交叉链接器，无需 Docker/cross：
#       aarch64:  sudo apt install gcc-aarch64-linux-gnu
#       armv7:    sudo apt install gcc-arm-linux-gnueabihf
#       i686:     sudo apt install gcc-i686-linux-gnu
#       windows:  sudo apt install gcc-mingw-w64-x86-64
#   - macOS 目标需要 Apple SDK，无法从 Linux 交叉编译：
#     推 tag（git tag v0.1.0 && git push --tags）由 GitHub Actions
#     release 工作流出全平台包，含 mac arm64/x64 与 Windows MSVC。
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=${LSCREEN_VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)}
HOST=$(rustc -vV | sed -n 's/^host: //p')
DIST=dist

DEFAULT_TARGETS=(
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    armv7-unknown-linux-gnueabihf
    i686-unknown-linux-gnu
    x86_64-pc-windows-gnu
)

# 交叉链接器（本机目标返回空 = 用默认链接器）
linker_for() {
    case "$1" in
        "$HOST") echo "" ;;
        aarch64-unknown-linux-gnu)     echo aarch64-linux-gnu-gcc ;;
        armv7-unknown-linux-gnueabihf) echo arm-linux-gnueabihf-gcc ;;
        i686-unknown-linux-gnu)        echo i686-linux-gnu-gcc ;;
        x86_64-pc-windows-gnu)         echo x86_64-w64-mingw32-gcc ;;
        *) echo "" ;;
    esac
}

# 本机能否构建该目标；不能时输出原因/安装提示到 stderr
usable() {
    local t=$1 linker
    if [[ $t == *apple-darwin* && $HOST != *apple-darwin* ]]; then
        echo "  跳过 $t：macOS 目标需 Apple SDK，请走 GitHub Actions release 工作流" >&2
        return 1
    fi
    if [[ $t == *windows-msvc* && $HOST != *windows* ]]; then
        echo "  跳过 $t：MSVC 目标只能在 Windows 上构建（本地可用 x86_64-pc-windows-gnu 代替）" >&2
        return 1
    fi
    linker=$(linker_for "$t")
    if [[ -n $linker ]] && ! command -v "$linker" >/dev/null; then
        echo "  跳过 $t：缺少交叉链接器 $linker（见脚本头部安装提示）" >&2
        return 1
    fi
    return 0
}

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

build_and_pack() {
    local t=$1 linker bin name stage out
    echo "==> 构建 $t"
    rustup target add "$t" >/dev/null 2>&1 || true

    linker=$(linker_for "$t")
    if [[ -n $linker ]]; then
        # CARGO_TARGET_<TRIPLE>_LINKER，三元组转大写、'-' 转 '_'
        local var="CARGO_TARGET_$(echo "$t" | tr 'a-z-' 'A-Z_')_LINKER"
        export "$var=$linker"
    fi
    cargo build --release --target "$t"

    bin="target/$t/release/lscreen"
    [[ $t == *windows* ]] && bin="$bin.exe"
    name="lscreen-v$VERSION-$t"
    stage="$DIST/.stage/$name"
    rm -rf "$stage" && mkdir -p "$stage"
    cp "$bin" README.md LICENSE "$stage/"

    if [[ $t == *windows* ]]; then
        out="$DIST/$name.zip"
        rm -f "$out"
        if command -v zip >/dev/null; then
            (cd "$stage" && zip -qr "$OLDPWD/$out" .)
        elif command -v 7z >/dev/null; then
            7z a -bso0 -bsp0 "$out" "$stage"/*
        else
            echo "错误：打 zip 需要 zip 或 7z" >&2
            exit 1
        fi
    else
        out="$DIST/$name.tar.gz"
        tar -czf "$out" -C "$DIST/.stage" "$name"
    fi
    echo "    $out ($(du -h "$out" | cut -f1))"
}

if [[ ${1:-} == --list ]]; then
    echo "默认目标集（host: $HOST）:"
    for t in "${DEFAULT_TARGETS[@]}"; do
        if usable "$t" 2>/dev/null; then
            echo "  可用   $t"
        else
            echo "  不可用 $t"
        fi
    done
    echo "macOS：aarch64/x86_64-apple-darwin 仅 GitHub Actions release 工作流可出包"
    exit 0
fi

if [[ $# -gt 0 ]]; then
    targets=("$@")
else
    targets=()
    for t in "${DEFAULT_TARGETS[@]}"; do
        usable "$t" && targets+=("$t")
    done
fi

if [[ ${#targets[@]} -eq 0 ]]; then
    echo "没有可构建的目标；按脚本头部提示安装交叉链接器后重试" >&2
    exit 1
fi

mkdir -p "$DIST"
for t in "${targets[@]}"; do
    usable "$t" || exit 1
    build_and_pack "$t"
done
rm -rf "$DIST/.stage"

# 汇总校验和（覆盖式重建，包含 dist 下全部既有包）
(
    cd "$DIST"
    archives=$(ls *.tar.gz *.zip 2>/dev/null || true)
    # shellcheck disable=SC2086
    [[ -n $archives ]] && sha256 $archives > SHA256SUMS
)
echo "==> 完成，产物在 $DIST/（校验和 $DIST/SHA256SUMS）"

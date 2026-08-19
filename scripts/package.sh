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

# ---------------- 平台原生包：deb / rpm / AppImage / 安装器 exe ----------------
# 对应工具缺失时跳过该格式并提示安装方式，不影响 tar.gz/zip 主产物。

deb_arch() { case "$1" in x86_64-*) echo amd64;; aarch64-*) echo arm64;; armv7-*) echo armhf;; i686-*) echo i386;; esac; }
rpm_arch() { case "$1" in x86_64-*) echo x86_64;; aarch64-*) echo aarch64;; armv7-*) echo armv7hl;; i686-*) echo i686;; esac; }
ai_arch()  { case "$1" in x86_64-*) echo x86_64;; aarch64-*) echo aarch64;; armv7-*) echo armhf;; i686-*) echo i686;; esac; }

# FHS 目录树：二进制 + desktop 入口 + 图标（deb/rpm/AppImage 共用）
stage_fhs() { # $1=根目录 $2=二进制
    install -Dm755 "$2" "$1/usr/bin/lscreen"
    install -Dm644 packaging/lscreen.desktop "$1/usr/share/applications/lscreen.desktop"
    install -Dm644 packaging/icon.png "$1/usr/share/icons/hicolor/256x256/apps/lscreen.png"
}

make_deb() { # $1=target $2=bin
    command -v dpkg-deb >/dev/null || { echo "    跳过 deb：无 dpkg-deb"; return; }
    local arch root out
    arch=$(deb_arch "$1")
    root="$DIST/.stage/deb-$1"
    rm -rf "$root" && stage_fhs "$root" "$2"
    mkdir -p "$root/DEBIAN"
    cat > "$root/DEBIAN/control" <<EOF
Package: lscreen
Version: $VERSION
Architecture: $arch
Maintainer: crazykun <crazykun@users.noreply.github.com>
Section: graphics
Priority: optional
Depends: libc6
Homepage: https://github.com/crazykun/LaterScreen
Description: Screenshot & annotation tool (LaterScreen)
 跨平台截图标注工具：截图、标注、取色、二维码识别、OCR。
EOF
    out="$DIST/lscreen_${VERSION}_$arch.deb"
    dpkg-deb --build --root-owner-group "$root" "$out" >/dev/null
    echo "    $out ($(du -h "$out" | cut -f1))"
}

make_rpm() { # $1=target $2=bin
    command -v rpmbuild >/dev/null || { echo "    跳过 rpm：无 rpmbuild（sudo apt install rpm）"; return; }
    local arch top out
    arch=$(rpm_arch "$1")
    top="$PWD/$DIST/.stage/rpmtop-$1"
    rm -rf "$top" && stage_fhs "$top/SOURCES/fhs" "$2"
    cat > "$top/lscreen.spec" <<EOF
Name: lscreen
Version: $VERSION
Release: 1
Summary: Screenshot & annotation tool (LaterScreen)
License: MIT
URL: https://github.com/crazykun/LaterScreen
AutoReqProv: no
%global debug_package %{nil}
%define __os_install_post %{nil}
%define _build_id_links none
%description
跨平台截图标注工具：截图、标注、取色、二维码识别、OCR。
%install
cp -a %{_sourcedir}/fhs/. %{buildroot}/
%files
/usr/bin/lscreen
/usr/share/applications/lscreen.desktop
/usr/share/icons/hicolor/256x256/apps/lscreen.png
EOF
    rpmbuild -bb --quiet --target "$arch" --define "_topdir $top" "$top/lscreen.spec" >/dev/null
    out="$DIST/lscreen-$VERSION-1.$arch.rpm"
    cp "$top/RPMS/$arch/lscreen-$VERSION-1.$arch.rpm" "$out"
    echo "    $out ($(du -h "$out" | cut -f1))"
}

make_appimage() { # $1=target $2=bin
    command -v appimagetool >/dev/null || {
        echo "    跳过 AppImage：无 appimagetool（github.com/AppImage/appimagetool releases）"
        return
    }
    local arch appdir out
    arch=$(ai_arch "$1")
    appdir="$DIST/.stage/AppDir-$1"
    rm -rf "$appdir" && stage_fhs "$appdir" "$2"
    ln -sf usr/bin/lscreen "$appdir/AppRun"
    cp packaging/lscreen.desktop "$appdir/"
    cp packaging/icon.png "$appdir/lscreen.png"
    ln -sf lscreen.png "$appdir/.DirIcon"
    out="$DIST/lscreen-v$VERSION-$arch.AppImage"
    # EXTRACT_AND_RUN：无 FUSE 环境（CI 容器）也能跑
    ARCH=$arch APPIMAGE_EXTRACT_AND_RUN=1 appimagetool "$appdir" "$out" >/dev/null
    echo "    $out ($(du -h "$out" | cut -f1))"
}

make_setup() { # $1=target $2=bin(已构建的 lscreen.exe)
    # 自绘安装器：把 lscreen.exe 内嵌进 lscreen-setup.exe 再构建
    # （build.rs 经 LSCREEN_EMBED_STAMP 指纹保证内容变化触发重编译）
    local out setup_bin
    out="$DIST/lscreen-v$VERSION-$1-setup.exe"
    setup_bin="target/$1/release/lscreen-setup.exe"
    LSCREEN_BIN="$PWD/$2" cargo build --release -p lscreen-setup --target "$1" --quiet
    cp "$setup_bin" "$out"
    echo "    $out ($(du -h "$out" | cut -f1))"
}

make_dmg() { # $1=target $2=bin
    [[ $HOST == *apple-darwin* ]] && command -v hdiutil >/dev/null || {
        echo "    跳过 dmg：仅能在 macOS 上制作（hdiutil），CI 的 macOS 矩阵会出"
        return
    }
    local stage="$DIST/.stage/dmg-$1" app out
    app="$stage/LaterScreen.app"
    rm -rf "$stage"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    install -m755 "$2" "$app/Contents/MacOS/lscreen"
    sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$app/Contents/Info.plist"
    # png -> icns（sips/iconutil 为 macOS 自带；256 源图放大到 512 可接受）
    local iconset="$DIST/.stage/icon.iconset"
    rm -rf "$iconset" && mkdir -p "$iconset"
    local s
    for s in 16 32 64 128 256; do
        sips -z "$s" "$s" packaging/icon.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
        sips -z "$((s * 2))" "$((s * 2))" packaging/icon.png \
            --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/icon.icns"
    # 拖拽安装：卷内放 /Applications 快捷方式
    ln -s /Applications "$stage/Applications"
    out="$DIST/lscreen-v$VERSION-$1.dmg"
    rm -f "$out"
    hdiutil create -quiet -volname "LaterScreen" -srcfolder "$stage" -format UDZO "$out"
    echo "    $out ($(du -h "$out" | cut -f1))"
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

    # 平台原生包
    if [[ $t == *linux* ]]; then
        make_deb "$t" "$bin"
        make_rpm "$t" "$bin"
        make_appimage "$t" "$bin"
    elif [[ $t == *windows* ]]; then
        make_setup "$t" "$bin"
    elif [[ $t == *apple-darwin* ]]; then
        make_dmg "$t" "$bin"
    fi
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
    archives=$(ls *.tar.gz *.zip *.deb *.rpm *.AppImage *.exe *.dmg 2>/dev/null || true)
    # shellcheck disable=SC2086
    [[ -n $archives ]] && sha256 $archives > SHA256SUMS
)
echo "==> 完成，产物在 $DIST/（校验和 $DIST/SHA256SUMS）"

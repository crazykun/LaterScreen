#!/usr/bin/env bash
# LaterScreen 一键打包：构建目标平台的 release 二进制并归档到 dist/。
#
# 用法：
#   scripts/package.sh                 # 打包默认目标集中本机具备工具链的目标
#   scripts/package.sh <target>...     # 只打包指定目标（rustc 三元组）
#   scripts/package.sh --list          # 列出默认目标集与本机可用性
#
# 依赖说明：
#   - 项目链接期仅依赖 libc（X11/GL 运行时 dlopen），但 MP4 编码用的
#     openh264 是 C++ 源，交叉编译需 gcc + g++ 两套，无需 Docker/cross：
#       aarch64:  sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu
#       windows:  sudo apt install gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64
#     （armv7 / i686 已随 release 矩阵一起砍掉：32 位桌面架构 0 下载）
#   - macOS 目标需要 Apple SDK，无法从 Linux 交叉编译：
#     推 tag（git tag v0.1.0 && git push --tags）由 GitHub Actions
#     release 工作流出全平台包，含 mac universal 胖包（一个 dmg 双架构）
#     与 Windows MSVC。
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=${LSCREEN_VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)}
HOST=$(rustc -vV | sed -n 's/^host: //p')
DIST=dist

DEFAULT_TARGETS=(
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    x86_64-pc-windows-gnu
)

# 交叉链接器（本机目标返回空 = 用默认链接器）
linker_for() {
    case "$1" in
        "$HOST") echo "" ;;
        aarch64-unknown-linux-gnu)     echo aarch64-linux-gnu-gcc ;;
        x86_64-pc-windows-gnu)         echo x86_64-w64-mingw32-gcc ;;
        *) echo "" ;;
    esac
}

# 本机能否构建该目标；不能时输出原因/安装提示到 stderr
usable() {
    local t=$1 linker
    if [[ $t == *apple-darwin* && $HOST != *apple-darwin* ]]; then
        echo "  跳过 ${t}：macOS 目标需 Apple SDK，请走 GitHub Actions release 工作流" >&2
        return 1
    fi
    if [[ $t == *windows-msvc* && $HOST != *windows* ]]; then
        echo "  跳过 ${t}：MSVC 目标只能在 Windows 上构建（本地可用 x86_64-pc-windows-gnu 代替）" >&2
        return 1
    fi
    linker=$(linker_for "$t")
    if [[ -n $linker ]] && ! command -v "$linker" >/dev/null; then
        echo "  跳过 ${t}：缺少交叉链接器 ${linker}（见脚本头部安装提示）" >&2
        return 1
    fi
    # openh264（MP4）要 C++ 交叉编译器，缺了会在编译到一半才失败，这里提前拦
    if [[ -n $linker ]] && ! command -v "${linker%gcc}g++" >/dev/null; then
        echo "  跳过 ${t}：缺少交叉 C++ 编译器 ${linker%gcc}g++（见脚本头部安装提示）" >&2
        return 1
    fi
    return 0
}

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# ---------------- 平台原生包：deb / AppImage / 安装器 exe ----------------
# 对应工具缺失时跳过该格式并提示安装方式，不影响 tar.gz/zip 主产物。

deb_arch() { case "$1" in x86_64-*) echo amd64;; aarch64-*) echo arm64;; esac; }
ai_arch()  { case "$1" in x86_64-*) echo x86_64;; aarch64-*) echo aarch64;; esac; }

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

check_win_deps() { # $1=exe —— 校验 Windows 产物没有外部运行时 DLL 依赖
    # v0.5.0 的教训：MSVC 默认动态链接 CRT，产物依赖 VCRUNTIME140.dll，
    # 干净的 Windows 10 没装 VC++ 运行库就直接打不开。这类缺陷在打包机上
    # 永远测不出来（开发机装过运行库），只能靠扫描导入表在出包时拦住。
    #
    # 用 grep -a 扫字符串而非 objdump：本函数要在 Windows runner 的 git bash
    # 里跑，那里没有 binutils。注意必须带 -a，否则 grep 把 exe 当二进制文件
    # 直接跳过、静默返回「没找到」（实测假阴性）。
    local exe=$1 target=${2:-} bad=""
    for dll in VCRUNTIME140 VCRUNTIME140_1 MSVCP140 libstdc++-6 libgcc_s_seh-1 libwinpthread-1; do
        grep -aqi "$dll\.dll" "$exe" && bad="$bad $dll.dll"
    done
    [[ -z $bad ]] && return 0
    # MSVC 是发布目标（release.yml 的 Windows 矩阵只有它），必须硬失败；
    # windows-gnu 只是「本地没有 Windows 时的编译替身」（见 usable()），
    # 它的 libstdc++ 压不掉是已知取舍，给警告不阻断本地打包。
    if [[ $target == *msvc* || -z $target ]]; then
        echo "错误：$(basename "$exe") 依赖外部运行时 DLL:$bad" >&2
        echo "      违反「无动态库依赖、拷走即用」硬约束，用户机器上会缺 DLL 打不开。" >&2
        echo "      修复：确认 .cargo/config.toml 的 +crt-static 对该目标生效。" >&2
        exit 1
    fi
    echo "    警告：$(basename "$exe") 依赖$bad" >&2
    echo "          $target 产物仅供本地编译验证，装到干净 Windows 上会报「找不到" >&2
    echo "          libstdc++-6.dll」。发布包走 MSVC 目标（GitHub Actions 出）。" >&2
}

make_setup() { # $1=target $2=bin(已构建的 lscreen.exe)
    # 自绘安装器：把 lscreen.exe 内嵌进 lscreen-setup.exe 再构建
    # （build.rs 经 LSCREEN_EMBED_STAMP 指纹保证内容变化触发重编译）
    local out setup_bin suffix=""
    [[ $1 == *windows-gnu* ]] && suffix="-localtest"
    out="$DIST/lscreen-v$VERSION-$1$suffix-setup.exe"
    setup_bin="target/$1/release/lscreen-setup.exe"
    LSCREEN_BIN="$PWD/$2" cargo build --release -p lscreen-setup --target "$1" --quiet
    # 安装器自身也要能在干净系统上跑——它是用户双击的第一个 exe，
    # 缺 DLL 的话连"安装"这一步都到不了（内嵌的主程序已在上游校验过）
    check_win_deps "$setup_bin" "$1"
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

# tar.gz 通用归档：二进制 + README + LICENSE。macOS 各架构单包与 universal
# 的每架构附包共用；windows 走 zip，不经过这里。
pack_tarball() { # $1=target $2=bin
    local t=$1 bin=$2 name stage out
    name="lscreen-v$VERSION-$t"
    stage="$DIST/.stage/$name"
    rm -rf "$stage" && mkdir -p "$stage"
    cp "$bin" README.md LICENSE "$stage/"
    out="$DIST/$name.tar.gz"
    tar -czf "$out" -C "$DIST/.stage" "$name"
    echo "    $out ($(du -h "$out" | cut -f1))"
}

# macOS universal（arm64 + x86_64 胖二进制）：同一台 mac 交叉编两片后
# lipo 合一。dmg 用胖包——用户下载时不用挑架构；tar.gz 仍按架构各一份，
# 给明确知道自己机器的人留更小的选择。universal2-apple-darwin 是本脚本
# 的合成目标名，rustup/cargo 没有这个 triple，真实构建目标是两个切片。
build_mac_universal() {
    local s slices=() fat
    for s in aarch64-apple-darwin x86_64-apple-darwin; do
        echo "==> 构建 ${s}（universal 切片）"
        rustup target add "$s" >/dev/null 2>&1 || true
        cargo build --release --target "$s"
        pack_tarball "$s" "target/$s/release/lscreen"
        slices+=("target/$s/release/lscreen")
    done
    mkdir -p "$DIST/.stage"
    fat="$DIST/.stage/lscreen-fat"
    lipo -create "${slices[@]}" -output "$fat"
    lipo -info "$fat" | sed 's/^/    /'
    make_dmg universal2-apple-darwin "$fat"
}

build_and_pack() {
    local t=$1 linker bin name stage out
    echo "==> 构建 $t"

    if [[ $t == universal2-apple-darwin ]]; then
        build_mac_universal
        return
    fi

    rustup target add "$t" >/dev/null 2>&1 || true

    linker=$(linker_for "$t")
    if [[ -n $linker ]]; then
        # CARGO_TARGET_<TRIPLE>_LINKER，三元组转大写、'-' 转 '_'
        local var="CARGO_TARGET_$(echo "$t" | tr 'a-z-' 'A-Z_')_LINKER"
        export "$var=$linker"
    fi
    cargo build --release --target "$t"

    bin="target/$t/release/lscreen"
    if [[ $t == *windows* ]]; then
        bin="$bin.exe"
        check_win_deps "$bin" "$t"
        # windows-gnu 是「本地没有 Windows 时的编译替身」，产物带 libstdc++-6.dll
        # 外部依赖（openh264 的 C++ 侧，mingw 下压不掉，详见 .cargo/config.toml）。
        # 文件名打上 -localtest：check_win_deps 的警告会淹在构建输出里，但 dist/
        # 下的文件名不会——实测有人装了本地 GNU 包，在干净虚拟机上报缺 DLL。
        name="lscreen-v$VERSION-$t"
        [[ $t == *windows-gnu* ]] && name="$name-localtest"
        stage="$DIST/.stage/$name"
        rm -rf "$stage" && mkdir -p "$stage"
        cp "$bin" README.md LICENSE "$stage/"
        out="$DIST/$name.zip"
        rm -f "$out"
        if command -v zip >/dev/null; then
            (cd "$stage" && zip -qr "$OLDPWD/$out" .)
        elif command -v 7z >/dev/null; then
            # 必须先 cd 进 stage：`7z a out "$stage"/*` 会把 dist/.stage/<name>/
            # 整条路径写进归档，用户解压出四层空目录套一个 exe（v0.5.1 的
            # windows-msvc.zip 实际如此——Windows runner 没有 zip，走的这条分支）。
            (cd "$stage" && 7z a -bso0 -bsp0 "$OLDPWD/$out" .)
        else
            echo "错误：打 zip 需要 zip 或 7z" >&2
            exit 1
        fi
        echo "    $out ($(du -h "$out" | cut -f1))"
    else
        pack_tarball "$t" "$bin"
    fi

    # 平台原生包
    if [[ $t == *linux* ]]; then
        make_deb "$t" "$bin"
        make_appimage "$t" "$bin"
    elif [[ $t == *windows* ]]; then
        make_setup "$t" "$bin"
    elif [[ $t == *apple-darwin* ]]; then
        make_dmg "$t" "$bin"
    fi
}

if [[ ${1:-} == --list ]]; then
    echo "默认目标集（host: ${HOST}）:"
    for t in "${DEFAULT_TARGETS[@]}"; do
        if usable "$t" 2>/dev/null; then
            echo "  可用   $t"
        else
            echo "  不可用 $t"
        fi
    done
    echo "macOS：universal2-apple-darwin（arm64+x64 胖包）仅 GitHub Actions release 工作流可出包"
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
    archives=$(ls *.tar.gz *.zip *.deb *.AppImage *.exe *.dmg 2>/dev/null || true)
    # shellcheck disable=SC2086
    [[ -n $archives ]] && sha256 $archives > SHA256SUMS
)
echo "==> 完成，产物在 $DIST/（校验和 $DIST/SHA256SUMS）"

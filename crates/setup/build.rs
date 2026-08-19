//! 内嵌主程序：`LSCREEN_BIN` 指向已构建的 lscreen(.exe) 时复制进 OUT_DIR，
//! 由 src/app.rs `include_bytes!` 打进安装器；未设置时写占位文件，
//! 使 crate 在任何平台/任何 CI 阶段都能独立编译。
//!
//! 重编译链路（两环缺一不可）：
//! 1. rerun-if-changed 指向主程序文件本身——路径不变内容变（package.sh
//!    重新构建主程序后）也能触发 build script 重跑
//! 2. 内容指纹 LSCREEN_EMBED_STAMP 以 rustc-env 导出——cargo 的 build.rs
//!    重跑不会自动触发 crate 重编译，而 include_bytes! 的内容不进指纹，
//!    env 变化才让 crate 重编译、内嵌更新

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=LSCREEN_BIN");
    let out = env::var("OUT_DIR").unwrap();
    let dst = Path::new(&out).join("lscreen.bin");

    if let Ok(src) = env::var("LSCREEN_BIN") {
        if Path::new(&src).is_file() {
            println!("cargo:rerun-if-changed={src}");
        }
    }
    let bytes = match env::var("LSCREEN_BIN") {
        Ok(src) if Path::new(&src).is_file() => std::fs::read(&src).expect("读取 LSCREEN_BIN"),
        _ => b"placeholder".to_vec(),
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    println!("cargo:rustc-env=LSCREEN_EMBED_STAMP={}", hasher.finish());
    println!("cargo:rustc-env=LSCREEN_EMBED_LEN={}", bytes.len());
    std::fs::write(&dst, bytes).expect("写 OUT_DIR/lscreen.bin");
}

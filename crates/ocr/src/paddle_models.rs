//! PP-OCRv4 固定模型组合的缓存、校验与显式下载管理。

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{OcrError, Result};

pub(crate) const DET_FILE: &str = "ch_PP-OCRv4_det_mobile.onnx";
pub(crate) const REC_FILE: &str = "ch_PP-OCRv4_rec_mobile.onnx";
pub(crate) const DICT_FILE: &str = "ppocr_keys_v1.txt";
const MODEL_BASE_URL: &str = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master";

#[derive(Clone, Copy)]
struct ModelSpec {
    file: &'static str,
    remote: &'static str,
    size: u64,
    sha256: &'static str,
}

// ModelScope Sha256 元数据与原文件逐字节核对（2026-11-02）。
const MANIFEST: [ModelSpec; 3] = [
    ModelSpec {
        file: DET_FILE,
        remote: "onnx/PP-OCRv4/det/ch_PP-OCRv4_det_mobile.onnx",
        size: 4_745_517,
        sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
    },
    ModelSpec {
        file: REC_FILE,
        remote: "onnx/PP-OCRv4/rec/ch_PP-OCRv4_rec_mobile.onnx",
        size: 10_857_958,
        sha256: "48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b",
    },
    ModelSpec {
        file: DICT_FILE,
        remote: "paddle/PP-OCRv4/rec/ch_PP-OCRv4_rec_mobile/ppocr_keys_v1.txt",
        size: 26_249,
        sha256: "28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7",
    },
];

/// 只解析路径，不访问磁盘：绝对 XDG_CACHE_HOME 优先，否则
/// HOME / USERPROFILE 下的 .cache/lscreen/ppocr/v1。无可用根目录返回 None。
pub fn models_dir() -> Option<PathBuf> {
    cache_dir(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
    )
}

fn cache_dir(
    xdg: Option<OsString>,
    home: Option<OsString>,
    profile: Option<OsString>,
) -> Option<PathBuf> {
    let cache_root = xdg
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join(".cache"))
        })
        .or_else(|| {
            profile
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join(".cache"))
        })?;

    Some(cache_root.join("lscreen").join("ppocr").join("v1"))
}

pub fn models_present() -> bool {
    models_dir().is_some_and(|dir| models_present_in(&dir))
}

/// 逐文件轻量状态（按固定字节数判定，不做哈希）：设置面板的状态列表用。
/// `(文件名, 大小字节, 是否就绪)`；无法定位缓存目录时返回 None。
pub fn file_states() -> Option<[(&'static str, u64, bool); 3]> {
    let dir = models_dir()?;
    Some(file_states_in(&dir))
}

fn file_states_in(dir: &Path) -> [(&'static str, u64, bool); 3] {
    MANIFEST.map(|spec| {
        let ready = fs::metadata(dir.join(spec.file))
            .map(|m| m.is_file() && m.len() == spec.size)
            .unwrap_or(false);
        (spec.file, spec.size, ready)
    })
}

/// 全量完整性校验（SHA-256 + 字节数）当前缓存目录的三个模型文件。
/// 供设置面板「校验完整性」按钮在后台线程调用；结果只有通过/失败（失败
/// 文案带第一个不一致的文件名），不存在部分通过。
pub fn verify_installed_models() -> Result<()> {
    let dir = models_dir().ok_or_else(|| OcrError("无法定位用户缓存目录".into()))?;
    verify_models(&dir)
}

pub(crate) fn models_present_in(dir: &Path) -> bool {
    present_for_manifest(dir, &MANIFEST)
}

fn present_for_manifest(dir: &Path, manifest: &[ModelSpec]) -> bool {
    manifest.iter().all(|spec| {
        fs::metadata(dir.join(spec.file))
            .map(|metadata| metadata.is_file() && metadata.len() == spec.size)
            .unwrap_or(false)
    })
}

pub(crate) fn verify_models(dir: &Path) -> Result<()> {
    verify_manifest(dir, &MANIFEST)
}

fn verify_manifest(dir: &Path, manifest: &[ModelSpec]) -> Result<()> {
    for spec in manifest {
        verify_one(dir, spec)?;
    }
    Ok(())
}

fn verify_one(dir: &Path, spec: &ModelSpec) -> Result<()> {
    let path = dir.join(spec.file);
    let metadata = fs::metadata(&path).map_err(|error| {
        OcrError(format!(
            "模型文件 {} 不存在或不可读: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(OcrError(format!(
            "模型路径不是普通文件: {}",
            path.display()
        )));
    }
    if metadata.len() != spec.size {
        return Err(OcrError(format!(
            "模型文件 {} 大小不符（实际 {}，应为 {} 字节）",
            path.display(),
            metadata.len(),
            spec.size
        )));
    }

    let mut file = File::open(&path)
        .map_err(|error| OcrError(format!("打开模型文件 {} 失败: {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut read = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| OcrError(format!("读取模型文件 {} 失败: {error}", path.display())))?;
        if count == 0 {
            break;
        }
        read = read.saturating_add(count as u64);
        hasher.update(&buffer[..count]);
    }
    if read != spec.size {
        return Err(OcrError(format!(
            "模型文件 {} 大小不符（读取 {}，应为 {} 字节）",
            path.display(),
            read,
            spec.size
        )));
    }
    let actual = hex_digest(hasher.finalize());
    if actual != spec.sha256 {
        return Err(OcrError(format!(
            "模型文件 {} SHA-256 校验失败（实际 {actual}，应为 {}）",
            path.display(),
            spec.sha256
        )));
    }
    Ok(())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn download_models(progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let dir = models_dir().ok_or_else(|| OcrError("无法定位用户缓存目录".into()))?;
    download_models_to(&dir, progress)
}

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RESPONSE_HEADER_SIZE: usize = 64 * 1024;
const READ_BUFFER_SIZE: usize = 64 * 1024;
const PROGRESS_GRANULARITY: u64 = 256 * 1024;
static PART_COUNTER: AtomicU64 = AtomicU64::new(0);
static DOWNLOAD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn download_models_to(dir: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let lock = DOWNLOAD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| OcrError("模型下载锁已中毒".into()))?;
    download_models_locked(dir, &MANIFEST, MODEL_BASE_URL, &download_agent(), progress)
}

fn download_models_locked(
    dir: &Path,
    manifest: &[ModelSpec],
    base_url: &str,
    agent: &ureq::Agent,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|error| OcrError(format!("创建模型目录 {} 失败: {error}", dir.display())))?;
    let total = manifest.iter().try_fold(0u64, |sum, spec| {
        sum.checked_add(spec.size)
            .ok_or_else(|| OcrError("模型总大小溢出".into()))
    })?;
    let mut completed = 0u64;
    progress(0, total);

    for spec in manifest {
        let destination = dir.join(spec.file);
        if verify_one(dir, spec).is_ok() {
            completed = completed
                .checked_add(spec.size)
                .ok_or_else(|| OcrError("模型进度溢出".into()))?;
            progress(completed, total);
            continue;
        }

        download_one(
            &destination,
            spec,
            base_url,
            agent,
            completed,
            total,
            progress,
        )?;
        completed = completed
            .checked_add(spec.size)
            .ok_or_else(|| OcrError("模型进度溢出".into()))?;
    }
    Ok(())
}

fn download_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(3)
            .max_response_header_size(MAX_RESPONSE_HEADER_SIZE)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .timeout_connect(Some(DOWNLOAD_TIMEOUT))
            .timeout_recv_response(Some(DOWNLOAD_TIMEOUT))
            .timeout_recv_body(Some(DOWNLOAD_TIMEOUT))
            .build(),
    )
}

#[cfg(test)]
fn download_manifest(
    dir: &Path,
    manifest: &[ModelSpec],
    base_url: &str,
    agent: &ureq::Agent,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let lock = DOWNLOAD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| OcrError("模型下载锁已中毒".into()))?;
    download_models_locked(dir, manifest, base_url, agent, progress)
}

fn download_one(
    destination: &Path,
    spec: &ModelSpec,
    base_url: &str,
    agent: &ureq::Agent,
    completed: u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), spec.remote);
    let response = agent
        .get(&url)
        .header(
            "User-Agent",
            format!("LaterScreen/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| OcrError(format!("下载 {url} 失败: {error}")))?;
    if !response.status().is_success() {
        return Err(OcrError(format!(
            "下载 {url} 失败: HTTP {}",
            response.status()
        )));
    }
    if let Some(content_length) = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        if content_length != spec.size {
            return Err(OcrError(format!(
                "模型 {} 响应大小不符（实际 {}，应为 {} 字节）",
                spec.file, content_length, spec.size
            )));
        }
    }

    let mut part = PartFile::create(destination)?;
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(spec.size.saturating_add(1))
        .reader();
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; READ_BUFFER_SIZE];
    let mut received = 0u64;
    let mut reported = 0u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| OcrError(format!("下载 {} 时读取失败: {error}", spec.file)))?;
        if count == 0 {
            break;
        }
        let next = received
            .checked_add(count as u64)
            .ok_or_else(|| OcrError(format!("下载 {} 大小溢出", spec.file)))?;
        if next > spec.size {
            return Err(OcrError(format!(
                "模型 {} 大小不符（下载超量，超过 {} 字节）",
                spec.file, spec.size
            )));
        }
        part.file_mut()?
            .write_all(&buffer[..count])
            .map_err(|error| OcrError(format!("写入模型 {} 失败: {error}", spec.file)))?;
        hasher.update(&buffer[..count]);
        received = next;
        // 最多每 256KiB 报一次，避免小包响应淹没 UI 消息队列。每个文件的
        // 最后进度仅在校验及原子发布后发出，失败绝不报告 100%。
        if received < spec.size && received - reported >= PROGRESS_GRANULARITY {
            progress(completed + received, total);
            reported = received;
        }
    }
    if received != spec.size {
        return Err(OcrError(format!(
            "模型 {} 下载大小不符（实际 {}，应为 {} 字节）",
            spec.file, received, spec.size
        )));
    }
    let actual = hex_digest(hasher.finalize());
    if actual != spec.sha256 {
        return Err(OcrError(format!(
            "模型 {} SHA-256 校验失败（实际 {actual}，应为 {}）",
            spec.file, spec.sha256
        )));
    }
    part.file_mut()?
        .sync_all()
        .map_err(|error| OcrError(format!("刷新模型 {} 失败: {error}", spec.file)))?;
    part.publish(destination)?;
    progress(completed + spec.size, total);
    Ok(())
}

struct PartFile {
    path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl PartFile {
    fn create(destination: &Path) -> Result<Self> {
        let parent = destination
            .parent()
            .ok_or_else(|| OcrError(format!("模型路径无父目录: {}", destination.display())))?;
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| OcrError(format!("模型文件名无效: {}", destination.display())))?;
        let pid = std::process::id();
        for _ in 0..1024 {
            let counter = PART_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{file_name}.part.{pid}.{counter}"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(OcrError(format!(
                        "创建模型临时文件 {} 失败: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(OcrError(format!(
            "无法为 {} 创建唯一临时文件",
            destination.display()
        )))
    }

    fn file_mut(&mut self) -> Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| OcrError("模型临时文件已关闭".into()))
    }

    fn publish(&mut self, destination: &Path) -> Result<()> {
        // file_mut 的借用在这里已结束；先 close，再 rename，尤其重要于 Windows。
        self.file.take();
        fs::rename(&self.path, destination).map_err(|error| {
            OcrError(format!("发布模型 {} 失败: {error}", destination.display()))
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        // 先释放句柄再清理，避免 Windows 上删除打开的 .part 失败。
        self.file.take();
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    const FIXTURES: [ModelSpec; 3] = [
        ModelSpec {
            file: DET_FILE,
            remote: "det",
            size: 3,
            sha256: "3c2d493a01a75bfecc82d86ceb180a1318d50f3ec9ae1a8ca9eca8d26e527906",
        },
        ModelSpec {
            file: REC_FILE,
            remote: "rec",
            size: 3,
            sha256: "cb41b38387d6fca01272be03380f63568fccbd724c299ae58e8d548d91b67a58",
        },
        ModelSpec {
            file: DICT_FILE,
            remote: "dict",
            size: 4,
            sha256: "6ab47d70854a8c690a0c2035be903f3d812cbab06f9e442e9b10ad70b1acd446",
        },
    ];
    const BODIES: [&[u8]; 3] = [b"det", b"rec", b"dict"];

    struct Server {
        url: String,
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn new(mut respond: impl FnMut(&mut TcpStream, usize) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let hits = Arc::new(AtomicUsize::new(0));
            let thread_stop = Arc::clone(&stop);
            let thread_hits = Arc::clone(&hits);
            let thread = thread::spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            let mut request = Vec::new();
                            let mut buf = [0; 1024];
                            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                                match stream.read(&mut buf) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => request.extend_from_slice(&buf[..n]),
                                }
                                assert!(request.len() <= 16 * 1024);
                            }
                            let index = thread_hits.fetch_add(1, Ordering::Relaxed);
                            respond(&mut stream, index);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => panic!("本地测试服务器 accept 失败: {e}"),
                    }
                }
            });
            Self {
                url,
                hits,
                stop,
                thread: Some(thread),
            }
        }

        fn responses(responses: Vec<Vec<u8>>) -> Self {
            Self::new(move |stream, i| {
                let response = responses
                    .get(i)
                    .map(Vec::as_slice)
                    .unwrap_or(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
                // 下载器可提前拒绝超量响应，BrokenPipe 不属于测试服务器失败。
                let _ = stream.write_all(response);
            })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    fn response(body: &[u8], length: Option<u64>) -> Vec<u8> {
        let length = length
            .map(|n| format!("Content-Length: {n}\r\n"))
            .unwrap_or_default();
        [
            format!("HTTP/1.1 200 OK\r\n{length}Connection: close\r\n\r\n").as_bytes(),
            body,
        ]
        .concat()
    }

    fn agent(timeout: Duration) -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .proxy(None)
                .timeout_global(Some(timeout))
                .timeout_connect(Some(timeout))
                .timeout_recv_response(Some(timeout))
                .timeout_recv_body(Some(timeout))
                .build(),
        )
    }

    fn download(dir: &Path, server: &Server, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        download_manifest(
            dir,
            &FIXTURES,
            &server.url,
            &agent(Duration::from_secs(2)),
            progress,
        )
    }

    fn assert_no_parts(dir: &Path) {
        assert!(fs::read_dir(dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".part.")
        }));
    }

    #[test]
    fn cache_directory_is_optional_and_respects_absolute_xdg_without_creating_it() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let xdg = root.path().join("xdg");
        let profile = root.path().join("profile");
        assert_eq!(cache_dir(None, None, None), None);
        assert_eq!(
            cache_dir(Some("".into()), Some("".into()), Some("".into())),
            None
        );
        assert_eq!(
            cache_dir(Some(xdg.clone().into()), None, None),
            Some(xdg.join("lscreen/ppocr/v1"))
        );
        assert_eq!(
            cache_dir(
                None,
                Some(home.clone().into()),
                Some(profile.clone().into())
            ),
            Some(home.join(".cache/lscreen/ppocr/v1"))
        );
        assert_eq!(
            cache_dir(
                Some("relative".into()),
                Some("".into()),
                Some(profile.clone().into())
            ),
            Some(profile.join(".cache/lscreen/ppocr/v1"))
        );
        assert!(!home.exists());
        assert!(!xdg.exists());
        assert!(!profile.exists());
    }

    #[test]
    fn lightweight_presence_requires_three_nonempty_regular_files_of_exact_sizes() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("absent");
        assert!(!models_present_in(&dir));
        assert!(!dir.exists());
        assert!(verify_models(&dir).is_err());
        assert!(!dir.exists());
        fs::create_dir(&dir).unwrap();
        for entry in &MANIFEST {
            fs::File::create(dir.join(entry.file)).unwrap();
        }
        assert!(!models_present_in(&dir));
        for entry in &MANIFEST {
            fs::OpenOptions::new()
                .write(true)
                .open(dir.join(entry.file))
                .unwrap()
                .set_len(entry.size)
                .unwrap();
        }
        assert!(models_present_in(&dir));
        fs::remove_file(dir.join(REC_FILE)).unwrap();
        assert!(!models_present_in(&dir));
        fs::create_dir(dir.join(REC_FILE)).unwrap();
        assert!(!models_present_in(&dir));
    }

    #[test]
    fn file_states_report_each_manifest_entry_by_exact_size() {
        let dir = TempDir::new().unwrap();
        assert!(file_states_in(dir.path())
            .iter()
            .all(|(_, _, ready)| !ready));
        // 按清单字节数写入 fixture（哈希随意，本状态只看大小）
        for spec in &MANIFEST {
            fs::write(dir.path().join(spec.file), vec![0u8; spec.size as usize]).unwrap();
        }
        let states = file_states_in(dir.path());
        assert!(states.iter().all(|(_, _, ready)| *ready));
        // 其中一个截断 → 只有两个就绪，且文件名/大小与清单一致
        fs::write(dir.path().join(REC_FILE), b"short").unwrap();
        let states = file_states_in(dir.path());
        let rec = states
            .iter()
            .find(|(name, _, _)| *name == REC_FILE)
            .unwrap();
        assert!(!rec.2);
        assert_eq!(states.iter().filter(|(_, _, ready)| *ready).count(), 2);
        assert_eq!(
            states.map(|(name, size, _)| (name, size)),
            MANIFEST.map(|spec| (spec.file, spec.size))
        );
    }

    #[test]
    fn verification_hashes_existing_files_not_just_their_size() {
        let dir = TempDir::new().unwrap();
        for (spec, body) in FIXTURES.iter().zip(BODIES) {
            fs::write(dir.path().join(spec.file), body).unwrap();
        }
        verify_manifest(dir.path(), &FIXTURES).unwrap();
        fs::write(dir.path().join(REC_FILE), b"bad").unwrap();
        assert!(present_for_manifest(dir.path(), &FIXTURES));
        let err = verify_manifest(dir.path(), &FIXTURES).unwrap_err();
        assert!(err.0.contains("SHA-256"), "{err}");
        fs::write(dir.path().join(REC_FILE), b"").unwrap();
        let err = verify_manifest(dir.path(), &FIXTURES).unwrap_err();
        assert!(err.0.contains("大小"), "{err}");
    }

    #[test]
    fn downloads_publish_three_verified_files_with_monotonic_aggregate_progress() {
        let dir = TempDir::new().unwrap();
        let server = Server::responses(
            BODIES
                .iter()
                .map(|b| response(b, Some(b.len() as u64)))
                .collect(),
        );
        let mut events = Vec::new();
        download(dir.path(), &server, &mut |received, total| {
            events.push((received, total));
            assert_eq!(
                present_for_manifest(dir.path(), &FIXTURES),
                received == total
            );
        })
        .unwrap();
        assert_eq!(events, vec![(0, 10), (3, 10), (6, 10), (10, 10)]);
        for (spec, body) in FIXTURES.iter().zip(BODIES) {
            assert_eq!(fs::read(dir.path().join(spec.file)).unwrap(), body);
        }
        verify_manifest(dir.path(), &FIXTURES).unwrap();
        assert_eq!(server.hits.load(Ordering::Relaxed), 3);
        assert_no_parts(dir.path());
    }

    #[test]
    fn valid_existing_files_are_verified_and_reused_without_network() {
        let dir = TempDir::new().unwrap();
        for (spec, body) in FIXTURES.iter().zip(BODIES) {
            fs::write(dir.path().join(spec.file), body).unwrap();
        }
        let server = Server::responses(Vec::new());
        let mut events = Vec::new();
        download(dir.path(), &server, &mut |n, total| events.push((n, total))).unwrap();
        assert_eq!(server.hits.load(Ordering::Relaxed), 0);
        assert_eq!(events.last(), Some(&(10, 10)));
    }

    #[test]
    fn corrupted_existing_file_is_not_silently_skipped() {
        let dir = TempDir::new().unwrap();
        for (spec, body) in FIXTURES.iter().zip(BODIES) {
            fs::write(dir.path().join(spec.file), body).unwrap();
        }
        fs::write(dir.path().join(REC_FILE), b"bad").unwrap();
        let server = Server::responses(vec![response(b"rec", None)]);
        let mut events = Vec::new();
        download(dir.path(), &server, &mut |n, total| events.push((n, total))).unwrap();
        assert_eq!(fs::read(dir.path().join(REC_FILE)).unwrap(), b"rec");
        assert_eq!(server.hits.load(Ordering::Relaxed), 1);
        assert!(events.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(events.last(), Some(&(10, 10)));
        assert_no_parts(dir.path());
    }

    #[test]
    fn empty_truncated_oversized_and_wrong_hash_responses_never_replace_old_files() {
        let cases = [
            (response(b"", None), "大小"),
            (response(b"de", None), "大小"),
            (response(b"de", Some(3)), "下载"),
            (response(b"dett", None), "大小"),
            (response(b"", Some(0)), "大小"),
            (response(b"dett", Some(4)), "大小"),
            (response(b"bad", Some(3)), "SHA-256"),
        ];
        for (raw, expected_error) in cases {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join(DET_FILE), b"old").unwrap();
            let unrelated = dir.path().join("unrelated.part.123.456");
            fs::write(&unrelated, b"belongs to another downloader").unwrap();
            let server = Server::responses(vec![raw]);
            let mut events = Vec::new();
            let err =
                download(dir.path(), &server, &mut |n, total| events.push((n, total))).unwrap_err();
            assert!(err.0.contains(expected_error), "{err}");
            assert_eq!(fs::read(dir.path().join(DET_FILE)).unwrap(), b"old");
            assert!(!present_for_manifest(dir.path(), &FIXTURES));
            assert!(events.iter().all(|(n, total)| n < total));
            assert_eq!(
                fs::read(&unrelated).unwrap(),
                b"belongs to another downloader"
            );
            fs::remove_file(unrelated).unwrap();
            assert_no_parts(dir.path());
        }
    }

    #[test]
    fn failure_after_two_files_cannot_report_complete_or_leave_partial_dictionary() {
        let dir = TempDir::new().unwrap();
        let server = Server::responses(vec![
            response(b"det", None),
            response(b"rec", None),
            response(b"FAIL", None),
        ]);
        let mut events = Vec::new();
        let err =
            download(dir.path(), &server, &mut |n, total| events.push((n, total))).unwrap_err();
        assert!(err.0.contains("SHA-256"), "{err}");
        assert_eq!(fs::read(dir.path().join(DET_FILE)).unwrap(), b"det");
        assert_eq!(fs::read(dir.path().join(REC_FILE)).unwrap(), b"rec");
        assert!(!dir.path().join(DICT_FILE).exists());
        assert!(events.iter().all(|(n, total)| n < total));
        assert_no_parts(dir.path());
    }

    #[test]
    fn partial_download_is_not_visible_at_destination_and_probe_does_not_take_download_lock() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(DET_FILE), b"old").unwrap();
        let (sent_tx, sent_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let server = Server::new(move |stream, i| {
            if i == 0 {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nd",
                    )
                    .unwrap();
                sent_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                let _ = stream.write_all(b"et");
            } else {
                let _ = stream.write_all(&response(BODIES[i], None));
            }
        });
        let path = dir.path().to_path_buf();
        let url = server.url.clone();
        let worker = thread::spawn(move || {
            download_manifest(
                &path,
                &FIXTURES,
                &url,
                &agent(Duration::from_secs(5)),
                &mut |_, _| {},
            )
        });
        sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fs::read_dir(dir.path())
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().contains(".part."))
        {
            assert!(Instant::now() < deadline, "下载未创建唯一临时文件");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(fs::read(dir.path().join(DET_FILE)).unwrap(), b"old");
        let path = dir.path().to_path_buf();
        let (probe_tx, probe_rx) = mpsc::channel();
        let probe = thread::spawn(move || probe_tx.send(models_present_in(&path)).unwrap());
        assert!(!probe_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        probe.join().unwrap();
        resume_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(fs::read(dir.path().join(DET_FILE)).unwrap(), b"det");
        assert_no_parts(dir.path());
    }

    #[test]
    fn stalled_body_times_out_and_cleans_its_part() {
        let dir = TempDir::new().unwrap();
        let server = Server::new(|stream, _| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nd");
            thread::sleep(Duration::from_millis(150));
        });
        let err = download_manifest(
            dir.path(),
            &FIXTURES,
            &server.url,
            &agent(Duration::from_millis(50)),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(err.0.contains("下载"), "{err}");
        assert_eq!(server.hits.load(Ordering::Relaxed), 1);
        assert!(!dir.path().join(DET_FILE).exists());
        assert_no_parts(dir.path());
    }

    #[test]
    fn concurrent_downloads_share_one_process_wide_lock() {
        let dir = TempDir::new().unwrap();
        let (first_tx, first_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let server = Server::new(move |stream, i| {
            if i == 0 {
                first_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            let _ = stream.write_all(&response(BODIES[i % 3], None));
        });
        let (second_started_tx, second_started_rx) = mpsc::channel();
        let start = |signal: Option<mpsc::Sender<()>>| {
            let path = dir.path().to_path_buf();
            let url = server.url.clone();
            thread::spawn(move || {
                let mut signal = signal;
                download_manifest(
                    &path,
                    &FIXTURES,
                    &url,
                    &agent(Duration::from_secs(5)),
                    &mut |_, _| {
                        if let Some(signal) = signal.take() {
                            signal.send(()).unwrap();
                        }
                    },
                )
            })
        };
        let first = start(None);
        first_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = start(Some(second_started_tx));
        let was_locked = second_started_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err();
        resume_tx.send(()).unwrap();
        let first_result = first.join().unwrap();
        let second_result = second.join().unwrap();
        assert!(was_locked, "第二个下载未等待全进程锁");
        first_result.unwrap();
        second_result.unwrap();
        assert_eq!(server.hits.load(Ordering::Relaxed), 3);
        assert_no_parts(dir.path());
    }

    #[test]
    fn progress_callback_count_is_bounded_for_tiny_network_chunks() {
        // 固定小 fixture，服务器故意一块只发 128 字节。
        const BODY: [u8; 4096] = [b'x'; 4096];
        let digest = hex_digest(Sha256::digest(BODY));
        let spec = [ModelSpec {
            file: DET_FILE,
            remote: "det",
            size: BODY.len() as u64,
            sha256: Box::leak(digest.into_boxed_str()),
        }];
        let server = Server::new(|stream, _| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n");
            for chunk in BODY.chunks(128) {
                let _ = stream.write_all(chunk);
                thread::sleep(Duration::from_millis(2));
            }
        });
        let dir = TempDir::new().unwrap();
        let mut events = Vec::new();
        download_manifest(
            dir.path(),
            &spec,
            &server.url,
            &agent(Duration::from_secs(3)),
            &mut |n, total| events.push((n, total)),
        )
        .unwrap();
        assert!(events.len() <= 3, "进度回调未限量: {events:?}");
        assert_eq!(events.first(), Some(&(0, 4096)));
        assert_eq!(events.last(), Some(&(4096, 4096)));
        assert_no_parts(dir.path());
    }

    #[test]
    fn rename_failure_preserves_existing_directory_and_cleans_temporary_file() {
        let dir = TempDir::new().unwrap();
        let destination = dir.path().join(DET_FILE);
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), b"old").unwrap();
        let server = Server::responses(vec![response(b"det", None)]);
        let mut events = Vec::new();
        assert!(download(dir.path(), &server, &mut |n, total| events.push((n, total))).is_err());
        assert_eq!(fs::read(destination.join("keep")).unwrap(), b"old");
        assert!(events.iter().all(|(n, _)| *n == 0));
        assert_no_parts(dir.path());
    }

    #[test]
    #[ignore = "显式 LSCREEN_TEST_MODEL_DOWNLOAD=1 才从 HTTPS 下载到独立临时目录"]
    fn real_model_download_smoke_uses_only_temporary_cache() {
        if std::env::var("LSCREEN_TEST_MODEL_DOWNLOAD").as_deref() != Ok("1") {
            return;
        }
        let dir = TempDir::new().unwrap();
        let mut events = Vec::new();
        download_models_to(dir.path(), &mut |n, total| events.push((n, total))).unwrap();
        assert!(models_present_in(dir.path()));
        verify_models(dir.path()).unwrap();
        assert!(events.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(events.last(), Some(&(15_629_724, 15_629_724)));
        assert_no_parts(dir.path());
    }

    #[test]
    fn manifest_pins_exact_model_combination_and_https_sources() {
        assert_eq!(MANIFEST.iter().map(|m| m.size).sum::<u64>(), 15_629_724);
        assert!(MODEL_BASE_URL.starts_with("https://"));
        assert!(MANIFEST.iter().all(|m| m.sha256.len() == 64));
        assert!(download_agent().config().https_only());
    }
}

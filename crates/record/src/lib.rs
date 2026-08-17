//! lscreen-record: 屏幕录制编码。
//!
//! GIF：gifski（纯 Rust，帧差分 + 高质量调色板）。
//! MP4：计划走系统编码器（Win MF / mac VideoToolbox / Linux openh264），M4b。
//!
//! 帧源以闭包注入（返回 RGBA 帧），本 crate 不依赖截屏实现，便于单测与复用。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use imgref::ImgVec;
use rgb::RGBA8;

#[derive(Debug)]
pub struct RecordError(pub String);

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RecordError {}

pub type Result<T> = std::result::Result<T, RecordError>;

fn err<E: std::fmt::Display>(e: E) -> RecordError {
    RecordError(e.to_string())
}

pub struct GifOptions {
    /// 帧率，录屏合理范围 5..=30
    pub fps: u32,
    /// 1-100，gifski 推荐 50-100
    pub quality: u8,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self { fps: 10, quality: 90 }
    }
}

/// 按 fps 采帧编码 GIF，直到时长耗尽或 stop 置位。返回帧数。
///
/// grab_frame 返回 (rgba, width, height)；尺寸必须每帧一致（gifski 会缩放不一致帧，
/// 但录屏场景尺寸不一致说明上游有 bug，这里直接报错）。
pub fn record_gif(
    mut grab_frame: impl FnMut() -> Result<(Vec<u8>, u32, u32)>,
    opts: &GifOptions,
    max_duration: Duration,
    stop: &AtomicBool,
    out_path: &Path,
) -> Result<usize> {
    let fps = opts.fps.clamp(1, 30);
    let settings = gifski::Settings {
        width: None,
        height: None,
        quality: opts.quality.clamp(1, 100),
        fast: false,
        repeat: gifski::Repeat::Infinite,
    };
    let (collector, writer) = gifski::new(settings).map_err(err)?;

    let file = BufWriter::new(File::create(out_path).map_err(err)?);
    // 编码在独立线程消费帧队列，采帧循环不被编码耗时拖慢
    let encode_thread = std::thread::spawn(move || -> Result<()> {
        writer
            .write(file, &mut gifski::progress::NoProgress {})
            .map_err(err)
    });

    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let start = Instant::now();
    let mut frame_size: Option<(u32, u32)> = None;
    let mut count = 0usize;

    while !stop.load(Ordering::Relaxed) && start.elapsed() < max_duration {
        let tick = Instant::now();
        let (rgba, w, h) = grab_frame()?;
        match frame_size {
            None => frame_size = Some((w, h)),
            Some(size) if size != (w, h) => {
                return Err(RecordError(format!(
                    "帧尺寸不一致: {size:?} -> {:?}",
                    (w, h)
                )));
            }
            _ => {}
        }
        let pixels: Vec<RGBA8> = rgba
            .chunks_exact(4)
            .map(|p| RGBA8::new(p[0], p[1], p[2], 255))
            .collect();
        let pts = start.elapsed().as_secs_f64();
        collector
            .add_frame_rgba(count, ImgVec::new(pixels, w as usize, h as usize), pts)
            .map_err(err)?;
        count += 1;

        if let Some(rest) = interval.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    drop(collector); // 关闭帧队列，编码线程随之收尾
    encode_thread
        .join()
        .map_err(|_| RecordError("编码线程崩溃".into()))??;

    if count == 0 {
        return Err(RecordError("未采集到任何帧".into()));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成帧 → GIF 全链路：渐变色动画，验证产物是合法 GIF。
    #[test]
    fn synthetic_frames_to_gif() {
        let dir = std::env::temp_dir().join("lscreen-record-test.gif");
        let mut tick = 0u8;
        let stop = AtomicBool::new(false);
        let n = record_gif(
            move || {
                tick = tick.wrapping_add(40);
                let (w, h) = (64u32, 48u32);
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        rgba.extend_from_slice(&[
                            (x * 4) as u8,
                            (y * 5) as u8,
                            tick,
                            255,
                        ]);
                    }
                }
                Ok((rgba, w, h))
            },
            &GifOptions { fps: 30, quality: 60 },
            Duration::from_millis(200),
            &stop,
            &dir,
        )
        .unwrap();
        assert!(n >= 2, "帧数过少: {n}");
        let data = std::fs::read(&dir).unwrap();
        assert_eq!(&data[..6], b"GIF89a");
        std::fs::remove_file(&dir).ok();
    }
}

//! Windows / macOS 截屏：委托 xcap（这两个平台上走系统 API，无 C 编译依赖）。

use crate::{CaptureError, Result, Screenshot};
use xcap::Monitor;

fn err<E: std::fmt::Display>(e: E) -> CaptureError {
    CaptureError(e.to_string())
}

fn shoot(monitor: &Monitor) -> Result<Screenshot> {
    let img = monitor.capture_image().map_err(err)?;
    let (width, height) = (img.width(), img.height());
    Ok(Screenshot {
        rgba: img.into_raw(),
        width,
        height,
        origin: (monitor.x().map_err(err)?, monitor.y().map_err(err)?),
        scale: monitor.scale_factor().map_err(err)?,
        is_primary: monitor.is_primary().unwrap_or(false),
    })
}

pub fn capture_primary() -> Result<Screenshot> {
    let monitors = Monitor::all().map_err(err)?;
    let primary = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| CaptureError("no monitor found".into()))?;
    shoot(primary)
}

pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    shoot(&Monitor::from_point(x, y).map_err(err)?)
}

pub fn capture_all() -> Result<Vec<Screenshot>> {
    Monitor::all().map_err(err)?.iter().map(shoot).collect()
}

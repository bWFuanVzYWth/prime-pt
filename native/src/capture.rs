//! 显式环境变量启用的一次性捕获；默认路径不保留副本、不访问文件系统。
use crate::Error;
use std::path::PathBuf;

pub(crate) struct Capture {
    pub directory: PathBuf,
    remaining: u32,
}
impl Capture {
    pub fn from_env() -> Result<Option<Self>, Error> {
        let Some(directory) = std::env::var_os("PRIME_PT_CAPTURE") else {
            return Ok(None);
        };
        let directory = PathBuf::from(directory);
        let remaining = std::env::var("PRIME_PT_CAPTURE_FRAMES")
            .unwrap_or_else(|_| "600".into())
            .parse::<u32>()
            .map_err(|_| Error::Input("PRIME_PT_CAPTURE_FRAMES 须为正整数"))?;
        if remaining == 0 {
            return Err(Error::Input("PRIME_PT_CAPTURE_FRAMES 须为正整数"));
        }
        std::fs::create_dir(&directory).map_err(|source| Error::RuntimePath {
            path: directory.clone(),
            source,
        })?;
        eprintln!(
            "[prime_pt] scene_capture enabled path={} frames={remaining}",
            directory.display()
        );
        Ok(Some(Self {
            directory,
            remaining,
        }))
    }
    pub fn frame(&mut self) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.remaining == 0
    }
}

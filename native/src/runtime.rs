//! 原生会话首次创建前固定发行资源来源；开发入口沿用源码树。

use crate::Error;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(crate) fn configure(path: &Path) -> Result<(), Error> {
    if !path.is_absolute() {
        return Err(Error::Input("发行资源根须为绝对路径"));
    }
    let root = fs::canonicalize(path).map_err(|source| Error::RuntimePath {
        path: path.to_owned(),
        source,
    })?;
    for relative in [
        "slang/slangc.exe",
        "shaders/prime/primary.slang",
        "shaders/engine/pt/scene.slang",
    ] {
        let path = root.join(relative);
        let metadata = fs::metadata(&path).map_err(|source| Error::RuntimePath {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(Error::RuntimePath {
                path,
                source: io::Error::new(io::ErrorKind::InvalidInput, "须为普通文件"),
            });
        }
    }
    match ROOT.set(Some(root)) {
        Ok(()) => Ok(()),
        Err(attempt) if ROOT.get() == Some(&attempt) => Ok(()),
        Err(_) => Err(Error::Input("发行资源来源已固定")),
    }
}

pub(crate) fn root() -> Option<&'static Path> {
    ROOT.get_or_init(|| None).as_deref()
}

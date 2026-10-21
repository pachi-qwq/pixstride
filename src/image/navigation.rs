//! 扫描当前目录并维护确定性的图片浏览顺序。

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::is_supported_path;

/// 一个目录内按文件名排序的图片集合及当前选择。
#[derive(Debug)]
pub(crate) struct ImageDirectory {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) selected: usize,
}

impl ImageDirectory {
    /// 扫描目录，按 Unicode 小写文件名排序，并以原始 `OsString` 打破同名大小写平局。
    pub(crate) fn scan(directory: &Path, current: &Path) -> Result<Self> {
        let entries = fs::read_dir(directory)
            .with_context(|| format!("无法扫描图片目录：{}", directory.display()))?;
        let mut files = Vec::new();
        for entry in entries {
            let entry =
                entry.with_context(|| format!("读取目录条目失败：{}", directory.display()))?;
            if !entry
                .file_type()
                .with_context(|| format!("读取文件类型失败：{}", entry.path().display()))?
                .is_file()
            {
                continue;
            }
            let path = entry.path();
            if is_supported_path(&path) {
                files.push(path);
            }
        }
        files.sort_by_cached_key(|path| {
            let name = path.file_name().unwrap_or_default();
            (name.to_string_lossy().to_lowercase(), name.to_os_string())
        });

        let current_key = path_key(current);
        let selected = files
            .iter()
            .position(|path| path_key(path) == current_key)
            .context("当前图片在目录扫描期间已被删除或改名")?;
        Ok(Self { files, selected })
    }

    /// 沿指定方向移动一个位置；到达目录边界时保留当前选择。
    pub(crate) fn step(&mut self, delta: isize) -> bool {
        let Some(next) = self.selected.checked_add_signed(delta) else {
            return false;
        };
        if next >= self.files.len() {
            return false;
        }
        self.selected = next;
        true
    }

    pub(crate) fn current(&self) -> &Path {
        &self.files[self.selected]
    }

    /// 返回至多两个相邻项，顺序固定为下一张、上一张。
    pub(crate) fn neighbors(&self) -> Vec<PathBuf> {
        let mut neighbors = Vec::with_capacity(2);
        if self.selected + 1 < self.files.len() {
            neighbors.push(self.files[self.selected + 1].clone());
        }
        if self.selected > 0 {
            neighbors.push(self.files[self.selected - 1].clone());
        }
        neighbors
    }
}

pub(crate) fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()
            .context("读取当前工作目录失败")?
            .join(path))
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{ImageDirectory, absolute_path};
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            fs::create_dir_all(&root).unwrap();
            let path = root.join(format!("pixstride-nav-{unique}"));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"fixture").unwrap();
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_sorts_case_insensitively_with_stable_tie_break_and_filters_extensions() {
        let directory = TestDirectory::new();
        let z = directory.file("zeta.jpg");
        let upper = directory.file("Alpha.PNG");
        let lower = directory.file("alpha.bmp");
        directory.file("notes.txt");
        let gif = directory.file("animation.gif");

        let result = ImageDirectory::scan(&directory.0, &lower).unwrap();
        assert_eq!(result.files, vec![lower, upper, gif, z]);
    }

    #[test]
    fn navigation_stops_at_directory_edges_and_returns_neighbors() {
        let directory = TestDirectory::new();
        let a = directory.file("a.jpg");
        let b = directory.file("b.png");
        let c = directory.file("c.bmp");
        let mut result = ImageDirectory::scan(&directory.0, &b).unwrap();
        assert_eq!(result.neighbors(), vec![c.clone(), a.clone()]);
        assert!(result.step(1));
        assert_eq!(result.current(), c);
        assert!(!result.step(1));
        assert!(result.step(-1));
        assert_eq!(result.current(), b);
        assert!(result.step(-1));
        assert_eq!(result.current(), a);
        assert!(!result.step(-1));
    }

    #[test]
    fn missing_selected_file_is_reported() {
        let directory = TestDirectory::new();
        directory.file("present.png");
        assert!(ImageDirectory::scan(&directory.0, Path::new("missing.png")).is_err());
    }

    #[test]
    fn relative_open_path_becomes_absolute_without_changing_cwd() {
        let absolute = absolute_path(Path::new("folder name\u{00e9}.png")).unwrap();
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(OsString::from("folder name\u{00e9}.png")));
    }
}

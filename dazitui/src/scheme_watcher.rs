//! 方案源文件监控封装（基于 `notify`）。
//!
//! 把 `notify` 的跨平台文件监控能力收敛成一个极简、可单测的组件，供方案热重载
//! （issue #91 / #93）使用。它只回答一个问题：自上次询问以来，被监控的方案源文件里
//! 有没有发生改动？
//!
//! ## 设计要点
//!
//! 1. **监控父目录而非文件本身**：inotify 按 inode 监控，编辑器「原子保存」（写临时文件再
//!    rename 覆盖）会让原 inode 失效、监控随之静默丢失。改为以 `NonRecursive` 监控每个源文件
//!    所在目录，再用「事件路径是否落在被监控文件集合」做二次过滤，既覆盖原子保存，也不会因
//!    同目录下的其他文件改动而误触发。
//! 2. **路径一律 canonicalize**：`~/.config/dazitui/schemes` 是 `fcitx5/rime` 的软链，必须解析到
//!    真实文件，否则监控的是软链 inode，收不到真实改动。
//! 3. **`set_paths` 可重复调用**：先移除旧监控、再加入新监控，因此「切换方案」时能安全重建
//!    监控闭包。

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

/// 对 `notify` 的薄封装：监控一组规范化后的方案源文件，非阻塞地回答「刚刚是否改动」。
pub struct SchemeWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<Event>>,
    /// 已规范化的被监控文件路径集合（用于事件过滤）。
    watched_files: HashSet<PathBuf>,
    /// 实际以 `NonRecursive` 监控的父目录集合（用于 `unwatch`）。
    watched_dirs: HashSet<PathBuf>,
}

impl SchemeWatcher {
    /// 创建监控器。`notify` 初始化失败（如文件描述符耗尽）时返回 `Err`。
    pub fn new() -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let watcher = RecommendedWatcher::new(tx, Config::default())?;
        Ok(Self {
            watcher,
            receiver: rx,
            watched_files: HashSet::new(),
            watched_dirs: HashSet::new(),
        })
    }

    /// 重置监控目标为 `paths`。
    ///
    /// 每个路径先 canonicalize；内部以 `NonRecursive` 监控其所在目录，并把规范化的文件本身
    /// 记入 `watched_files` 用于事件过滤。旧目录先 `unwatch` 再丢弃；监控失败（路径不存在等）
    /// 的目录会被跳过，不阻断其余目录。
    pub fn set_paths(&mut self, paths: &[PathBuf]) {
        // 1. 移除旧监控。
        let old_dirs: Vec<PathBuf> = self.watched_dirs.drain().collect();
        for dir in &old_dirs {
            let _ = self.watcher.unwatch(dir);
        }
        self.watched_files.clear();

        // 2. 按规范化文件去重父目录后加入监控。
        //    监控父目录（NonRecursive）既能覆盖「原子保存替换 inode」，又因下方按文件集合
        //    过滤而不会因同目录其他文件改动而误触发。
        let mut dirs_to_watch: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            let canon = canonicalize(p);
            self.watched_files.insert(canon.clone());
            let dir = canon
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| canon.clone());
            dirs_to_watch.insert(dir);
        }
        for dir in dirs_to_watch {
            if self.watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                self.watched_dirs.insert(dir);
            }
        }
    }

    /// 非阻塞排空事件通道：当且仅当其中有「命中被监控文件集合」的事件时返回 `true`。
    /// 无事件、仅有未监控路径的事件、或 `notify` 内部错误事件时返回 `false`。
    pub fn drain_changed(&mut self) -> bool {
        let mut changed = false;
        while let Ok(res) = self.receiver.try_recv() {
            if let Ok(event) = res {
                for p in &event.paths {
                    let canon = canonicalize(p);
                    if self.watched_files.contains(&canon) {
                        changed = true;
                    }
                }
            }
        }
        changed
    }
}

/// 规范路径：解析软链到真实文件；失败则回退原路径（与 `SchemeDict::source_paths` 策略一致）。
fn canonicalize(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread::sleep;
    use std::time::Duration;

    /// 在 `dir` 下创建带初始内容的临时文件，返回其路径。
    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "initial").unwrap();
        path
    }

    /// 追加写入以触发 modify 事件，并 flush/sync 确保落到磁盘。
    fn modify(path: &Path) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "changed-{}", std::process::id()).unwrap();
        f.sync_all().unwrap();
    }

    /// 等事件经后台线程投递到 mpsc 通道（inotify 事件有极短同步窗口）。
    fn settle() {
        sleep(Duration::from_millis(80));
    }

    #[test]
    fn new_watcher_has_no_pending_changes() {
        let mut w = SchemeWatcher::new().unwrap();
        w.set_paths(&[]);
        assert!(!w.drain_changed(), "未监控任何文件时不应报告改动");
    }

    #[test]
    fn detects_modification_of_watched_file() {
        let dir = std::env::temp_dir().join(format!("dazitui_watch_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = touch(&dir, "a.dict.yaml");

        let mut w = SchemeWatcher::new().unwrap();
        w.set_paths(&[file.clone()]);
        settle();

        modify(&file);
        settle();

        assert!(w.drain_changed(), "应检测到被监控文件的修改");
        // 事件已排空，再次查询应返回 false。
        assert!(!w.drain_changed(), "事件被排空后不应重复报告");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_paths_replaces_previous_watch() {
        let base = std::env::temp_dir().join(format!("dazitui_watch_test2_{}", std::process::id()));
        let d1 = base.join("d1");
        let d2 = base.join("d2");
        let _ = std::fs::create_dir_all(&d1);
        let _ = std::fs::create_dir_all(&d2);
        let f1 = touch(&d1, "old.dict.yaml");
        let f2 = touch(&d2, "new.dict.yaml");

        let mut w = SchemeWatcher::new().unwrap();
        w.set_paths(&[f1.clone()]);
        // 切换到 f2：f1 的目录应被取消监控。
        w.set_paths(&[f2.clone()]);
        settle();

        // 改 f1（已不在监控内）不应触发。
        modify(&f1);
        settle();
        assert!(!w.drain_changed(), "已取消监控的文件不应触发改动");

        // 改 f2 应触发。
        modify(&f2);
        settle();
        assert!(w.drain_changed(), "当前监控的文件应触发改动");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ignores_sibling_file_in_same_directory() {
        let dir = std::env::temp_dir().join(format!("dazitui_watch_test3_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let watched = touch(&dir, "target.dict.yaml");
        let sibling = touch(&dir, "sibling.dict.yaml");

        let mut w = SchemeWatcher::new().unwrap();
        w.set_paths(&[watched.clone()]);
        settle();

        // 改同目录下的无关文件：不应触发。
        modify(&sibling);
        settle();
        assert!(!w.drain_changed(), "同目录无关文件的改动不应触发");

        // 改被监控文件：应触发。
        modify(&watched);
        settle();
        assert!(w.drain_changed(), "被监控文件改动应触发");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

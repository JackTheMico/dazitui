use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

/// Linux input_event 数据结构（标准 64 位 Linux 布局，大小为 24 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputEvent {
    pub time_sec: usize,
    pub time_usec: usize,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

pub const EV_KEY: u16 = 0x01;
pub const KEY_PRESS: i32 = 1;
pub const KEY_REPEAT: i32 = 2;

/// 将 Linux input-event-codes 映射到 LiveKeyboard 规范化键名。
pub fn evdev_code_to_key_name(code: u16) -> Option<&'static str> {
    match code {
        // 第一行数字与符号
        41 => Some("`"),
        2 => Some("1"),
        3 => Some("2"),
        4 => Some("3"),
        5 => Some("4"),
        6 => Some("5"),
        7 => Some("6"),
        8 => Some("7"),
        9 => Some("8"),
        10 => Some("9"),
        11 => Some("0"),
        12 => Some("-"),
        13 => Some("="),
        14 => Some("Backspace"),

        // 第二行 Q..P 与括号
        15 => Some("Tab"),
        16 => Some("q"),
        17 => Some("w"),
        18 => Some("e"),
        19 => Some("r"),
        20 => Some("t"),
        21 => Some("y"),
        22 => Some("u"),
        23 => Some("i"),
        24 => Some("o"),
        25 => Some("p"),
        26 => Some("["),
        27 => Some("]"),
        43 => Some("\\"),

        // 第三行 A..L 与分号引号
        58 => Some("Caps"),
        30 => Some("a"),
        31 => Some("s"),
        32 => Some("d"),
        33 => Some("f"),
        34 => Some("g"),
        35 => Some("h"),
        36 => Some("j"),
        37 => Some("k"),
        38 => Some("l"),
        39 => Some(";"),
        40 => Some("'"),
        28 => Some("Enter"),

        // 第四行 Z..M 与逗号句号斜杠
        42 | 54 => Some("Shift"),
        44 => Some("z"),
        45 => Some("x"),
        46 => Some("c"),
        47 => Some("v"),
        48 => Some("b"),
        49 => Some("n"),
        50 => Some("m"),
        51 => Some(","),
        52 => Some("."),
        53 => Some("/"),

        // 第五行与控制键
        29 | 97 => Some("Ctrl"),
        56 | 100 => Some("Alt"),
        57 => Some("Space"),
        1 => Some("Esc"),

        // 方向键
        103 => Some("Up"),
        105 => Some("Left"),
        106 => Some("Right"),
        108 => Some("Down"),

        _ => None,
    }
}

/// 通过 Linux sysfs capabilities/key 位图判断设备是否具备核心字母键区（A..Z）。
pub fn is_keyboard_device_sysfs(event_name: &str) -> bool {
    let cap_path = PathBuf::from("/sys/class/input")
        .join(event_name)
        .join("device/capabilities/key");
    if let Ok(content) = fs::read_to_string(&cap_path) {
        if let Some(last_word) = content.split_whitespace().last() {
            if let Ok(val) = u64::from_str_radix(last_word, 16) {
                // KEY_A = 30, KEY_Z = 44
                let has_key_a = (val & (1u64 << 30)) != 0;
                let has_key_z = (val & (1u64 << 44)) != 0;
                return has_key_a && has_key_z;
            }
        }
        return false;
    }
    // 若 sysfs 不可访问，做保守兜底允许打开
    true
}

/// 扫描系统中的物理键盘设备节点。
///
/// 扫描所有 `/dev/input/event*` 节点并通过 sysfs 键位位图精准识别全部有效键盘设备，
/// 包括 USB 键盘、蓝牙键盘（如 Cornix）以及笔记本内置键盘，并合并 `/dev/input/by-id` 符号链接。
pub fn discover_keyboard_devices() -> Vec<PathBuf> {
    let mut devices = Vec::new();

    // 1. 扫描 /dev/input/by-id/*-event-kbd 收集符号链接
    let by_id_dir = Path::new("/dev/input/by-id");
    if let Ok(entries) = fs::read_dir(by_id_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.ends_with("-event-kbd") {
                if let Ok(canonical) = entry.path().canonicalize() {
                    if !devices.contains(&canonical) {
                        devices.push(canonical);
                    }
                } else if !devices.contains(&entry.path()) {
                    devices.push(entry.path());
                }
            }
        }
    }

    // 2. 全量扫描 /dev/input/event* 并过滤出真正的打字键盘（解决蓝牙键盘无 by-id 符号链接的问题）
    let input_dir = Path::new("/dev/input");
    if let Ok(entries) = fs::read_dir(input_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with("event") && is_keyboard_device_sysfs(&name_str) {
                let path = entry.path();
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                if !devices.contains(&canonical) {
                    devices.push(canonical);
                }
            }
        }
    }

    devices
}

/// 从物理键盘接收实时击键的接收器句柄。
pub struct EvdevKeyReceiver {
    receiver: Receiver<(String, Instant)>,
    stop_flag: Arc<AtomicBool>,
    /// 是否至少成功打开了一个物理键盘设备
    pub is_active: bool,
    /// 诊断或权限警告信息（若权限不足等）
    pub status_message: Option<String>,
}

impl EvdevKeyReceiver {
    /// 尝试启动后台物理键盘监听器。
    pub fn start() -> Self {
        let devices = discover_keyboard_devices();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        if devices.is_empty() {
            return Self {
                receiver: rx,
                stop_flag,
                is_active: false,
                status_message: Some("未检测到物理键盘设备 (/dev/input)".to_string()),
            };
        }

        let mut opened_count = 0;
        let mut permission_denied = false;

        for dev_path in devices {
            match File::open(&dev_path) {
                Ok(file) => {
                    opened_count += 1;
                    let thread_stop = Arc::clone(&stop_flag);
                    let thread_tx = tx.clone();
                    let thread_dev_path = dev_path.clone();

                    thread::Builder::new()
                        .name(format!(
                            "evdev-reader-{}",
                            thread_dev_path.file_name().unwrap_or_default().to_string_lossy()
                        ))
                        .spawn(move || {
                            run_device_read_loop(file, thread_tx, thread_stop);
                        })
                        .ok();
                }
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::PermissionDenied {
                        permission_denied = true;
                    }
                }
            }
        }

        if opened_count > 0 {
            Self {
                receiver: rx,
                stop_flag,
                is_active: true,
                status_message: None,
            }
        } else if permission_denied {
            Self {
                receiver: rx,
                stop_flag,
                is_active: false,
                status_message: Some(
                    "读取 /dev/input 权限不足 (建议将当前用户加入 input 组: sudo usermod -aG input $USER)"
                        .to_string(),
                ),
            }
        } else {
            Self {
                receiver: rx,
                stop_flag,
                is_active: false,
                status_message: Some("打开键盘设备失败，已降级为方案反查回放".to_string()),
            }
        }
    }

    /// 非阻塞收取当前所有已到达的物理击键。
    pub fn try_recv_keys(&self) -> Vec<(String, Instant)> {
        let mut keys = Vec::new();
        while let Ok(item) = self.receiver.try_recv() {
            keys.push(item);
        }
        keys
    }
}

impl Drop for EvdevKeyReceiver {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// 单个设备的阻塞读取循环（运行在独立工作线程中）。
fn run_device_read_loop(
    mut file: File,
    tx: Sender<(String, Instant)>,
    stop_flag: Arc<AtomicBool>,
) {
    let event_size = std::mem::size_of::<InputEvent>();
    let mut buffer = vec![0u8; event_size];

    while !stop_flag.load(Ordering::Relaxed) {
        match file.read_exact(&mut buffer) {
            Ok(()) => {
                let event: InputEvent = unsafe { std::ptr::read(buffer.as_ptr() as *const _) };
                if event.type_ == EV_KEY && (event.value == KEY_PRESS || event.value == KEY_REPEAT) {
                    if let Some(key_name) = evdev_code_to_key_name(event.code) {
                        if tx.send((key_name.to_string(), Instant::now())).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(_) => {
                // 读取失败（如拔出设备或被中断），退出线程
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evdev_code_mappings() {
        assert_eq!(evdev_code_to_key_name(16), Some("q"));
        assert_eq!(evdev_code_to_key_name(30), Some("a"));
        assert_eq!(evdev_code_to_key_name(44), Some("z"));
        assert_eq!(evdev_code_to_key_name(57), Some("Space"));
        assert_eq!(evdev_code_to_key_name(14), Some("Backspace"));
        assert_eq!(evdev_code_to_key_name(1), Some("Esc"));
        assert_eq!(evdev_code_to_key_name(9999), None);
    }

    #[test]
    fn test_input_event_size_is_24_bytes() {
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
    }

    #[test]
    fn test_is_keyboard_device_sysfs() {
        assert!(is_keyboard_device_sysfs("non_existent_event_9999"));
        if std::path::Path::new("/dev/input/event26").exists() {
            assert!(is_keyboard_device_sysfs("event26"));
        }
        if std::path::Path::new("/dev/input/event4").exists() {
            assert!(!is_keyboard_device_sysfs("event4"));
        }
    }

    #[test]
    fn test_discover_keyboard_devices_finds_all_keyboards() {
        let devs = discover_keyboard_devices();
        if std::path::Path::new("/dev/input/event26").exists() {
            assert!(
                devs.contains(&std::path::PathBuf::from("/dev/input/event26")),
                "discover_keyboard_devices() missed /dev/input/event26! Found only: {:?}",
                devs
            );
        }
    }
}

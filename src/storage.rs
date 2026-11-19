use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_F1, VK_F11,
};

const CF_UNICODETEXT: u32 = 13;
const DELETE_RETRY_MS: u64 = 30_000;
const PNG_SOFTWARE: &str = "CliScreenshot";
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

impl Hotkey {
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (flag, name) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_SHIFT, "Shift"),
            (MOD_ALT, "Alt"),
        ] {
            if self.modifiers & flag != 0 {
                parts.push(name.to_owned());
            }
        }
        parts.push(key_label(self.key));
        parts.join("+")
    }

    pub fn valid(self) -> bool {
        self.modifiers != 0
            && self.modifiers & !(MOD_CONTROL | MOD_SHIFT | MOD_ALT) == 0
            && keys().any(|key| key == self.key)
    }
}

pub fn keys() -> impl Iterator<Item = u32> {
    (b'A'..=b'Z')
        .chain(b'0'..=b'9')
        .map(u32::from)
        .chain(u32::from(VK_F1)..=u32::from(VK_F11))
}

pub fn key_label(key: u32) -> String {
    if (u32::from(VK_F1)..=u32::from(VK_F11)).contains(&key) {
        return format!("F{}", key - u32::from(VK_F1) + 1);
    }
    char::from_u32(key).unwrap_or('?').to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub hotkey: Hotkey,
    pub retention_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: Hotkey {
                modifiers: MOD_ALT | MOD_SHIFT,
                key: u32::from(b'S'),
            },
            retention_minutes: 10,
        }
    }
}

pub struct Storage {
    root: PathBuf,
    pub captures: PathBuf,
}

impl Storage {
    pub fn open() -> io::Result<Self> {
        let root = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?
            .join("CliScreenshot");
        let captures = root.join("captures");
        fs::create_dir_all(&captures)?;
        Ok(Self { root, captures })
    }

    pub fn load_settings(&self) -> io::Result<Settings> {
        let contents = match fs::read_to_string(self.root.join("settings.ini")) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(error) => return Err(error),
        };
        parse_settings(&contents)
    }

    pub fn save_settings(&self, settings: Settings) -> io::Result<()> {
        let temporary = self.root.join("settings.tmp");
        let destination = self.root.join("settings.ini");
        let result = (|| {
            let mut file = File::create(&temporary)?;
            write!(
                file,
                "modifiers={}\nkey={}\nretention_minutes={}\n",
                settings.hotkey.modifiers, settings.hotkey.key, settings.retention_minutes
            )?;
            file.sync_all()?;
            drop(file);
            if unsafe {
                MoveFileExW(
                    wide(&temporary).as_ptr(),
                    wide(&destination).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn save_png(&self, width: u32, height: u32, rgb: &[u8]) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.captures)?;
        let pid = std::process::id();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self
            .captures
            .join(format!("shot-{}-{pid}-{sequence}.part", now_ms()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| {
            let mut buffered = BufWriter::new(file);
            write_png(&mut buffered, width, height, rgb)?;
            buffered.flush()?;
            drop(buffered);
            let path = self
                .captures
                .join(format!("shot-{}-{pid}-{sequence}.png", now_ms()));
            if unsafe { MoveFileExW(wide(&temporary).as_ptr(), wide(&path).as_ptr(), 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn clean(&self, retention_minutes: u32) -> Option<u64> {
        clean_at(&self.captures, retention_minutes, now_ms())
    }
}

fn write_png(output: impl Write, width: u32, height: u32, rgb: &[u8]) -> io::Result<()> {
    let mut encoder = png::Encoder::new(output, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk("Software".to_owned(), PNG_SOFTWARE.to_owned())
        .map_err(io::Error::other)?;
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer.write_image_data(rgb).map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)
}

fn is_capture(path: &Path) -> io::Result<bool> {
    let file = BufReader::new(File::open(path)?);
    match png::Decoder::new(file).read_info() {
        Ok(reader) => Ok(reader
            .info()
            .uncompressed_latin1_text
            .iter()
            .any(|text| text.keyword == "Software" && text.text == PNG_SOFTWARE)),
        Err(png::DecodingError::IoError(error)) if error.kind() != io::ErrorKind::UnexpectedEof => {
            Err(error)
        }
        Err(_) => Ok(false),
    }
}

fn parse_settings(contents: &str) -> io::Result<Settings> {
    let mut fields = [None; 3];
    for (key, value) in contents.lines().filter_map(|line| line.split_once('=')) {
        let index = match key {
            "modifiers" => 0,
            "key" => 1,
            "retention_minutes" => 2,
            _ => continue,
        };
        fields[index] = value.parse().ok();
    }
    if let [Some(modifiers), Some(key), Some(retention_minutes)] = fields {
        let hotkey = Hotkey { modifiers, key };
        if hotkey.valid() && retention_minutes > 0 {
            return Ok(Settings {
                hotkey,
                retention_minutes,
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "无效的快捷键或保留时间",
    ))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn capture_timestamp(path: &Path) -> Option<u64> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_prefix("shot-")?
        .strip_suffix(".png")?;
    let mut fields = name.split('-');
    let timestamp = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    fields.next().is_none().then_some(timestamp)
}

fn clean_at(directory: &Path, minutes: u32, now: u64) -> Option<u64> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(DELETE_RETRY_MS),
    };
    let mut next = None;
    for entry in entries {
        let Ok(entry) = entry else {
            schedule(&mut next, DELETE_RETRY_MS);
            continue;
        };
        let path = entry.path();
        let Some(saved_at) = capture_timestamp(&path) else {
            continue;
        };
        match entry.file_type() {
            Ok(kind) if kind.is_file() => (),
            Ok(_) => continue,
            Err(_) => {
                schedule(&mut next, DELETE_RETRY_MS);
                continue;
            }
        }
        let remaining = saved_at
            .saturating_add(u64::from(minutes) * 60_000)
            .saturating_sub(now);
        if remaining > 0 {
            schedule(&mut next, remaining);
            continue;
        }
        let result = is_capture(&path).and_then(|is_capture| {
            if is_capture {
                fs::remove_file(&path)
            } else {
                Ok(())
            }
        });
        if result.is_err_and(|error| error.kind() != io::ErrorKind::NotFound) {
            schedule(&mut next, DELETE_RETRY_MS);
        }
    }
    next
}

fn schedule(next: &mut Option<u64>, delay: u64) {
    *next = Some(next.map_or(delay, |current| current.min(delay)));
}

pub fn copy_path(owner: HWND, path: &Path) -> io::Result<()> {
    let text = wide(path);
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * size_of::<u16>());
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let target = GlobalLock(memory).cast::<u16>();
            if target.is_null() {
                return Err(io::Error::last_os_error());
            }
            target.copy_from_nonoverlapping(text.as_ptr(), text.len());
            GlobalUnlock(memory);
            for attempt in 0..6 {
                if OpenClipboard(owner) != 0 {
                    let result = if EmptyClipboard() != 0
                        && !SetClipboardData(CF_UNICODETEXT, memory).is_null()
                    {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    };
                    CloseClipboard();
                    return result;
                }
                if attempt < 5 {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Err(io::Error::last_os_error())
        })();
        if result.is_err() {
            GlobalFree(memory);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_require_recognized_hotkey_and_positive_retention() {
        assert_eq!(
            parse_settings("modifiers=5\nkey=83\nretention_minutes=10\n").unwrap(),
            Settings::default()
        );
        assert!(parse_settings("modifiers=0\nkey=83\nretention_minutes=10\n").is_err());
        assert!(parse_settings("modifiers=6\nkey=123\nretention_minutes=10\n").is_err());
        assert!(parse_settings("modifiers=6\nkey=83\nretention_minutes=0\n").is_err());
    }

    #[test]
    fn cleanup_uses_original_time_and_only_generated_png_names() {
        let directory = std::env::temp_dir().join(format!(
            "CliScreenshot-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir(&directory).unwrap();
        let expired = directory.join("shot-100000-23-0.png");
        let current = directory.join("shot-100001-23-1.png");
        let unrelated = directory.join("其他图.png");
        let unmarked = directory.join("shot-100000-23-3.png");
        let fake_capture = directory.join("shot-100000-23-2.png");
        for path in [&expired, &current] {
            write_png(File::create(path).unwrap(), 1, 1, &[12, 34, 56]).unwrap();
        }
        fs::write(&unrelated, []).unwrap();
        let mut encoder = png::Encoder::new(File::create(&unmarked).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[12, 34, 56]).unwrap();
        writer.finish().unwrap();
        fs::create_dir(&fake_capture).unwrap();
        assert_eq!(clean_at(&directory, 2, 160_000), Some(60_000));
        assert_eq!(clean_at(&directory, 1, 160_000), Some(1));
        assert!(!expired.exists());
        assert!(
            current.is_file() && unrelated.is_file() && unmarked.is_file() && fake_capture.is_dir()
        );
        assert_eq!(clean_at(&directory, 1, 160_001), None);
        fs::remove_file(unrelated).unwrap();
        fs::remove_file(unmarked).unwrap();
        fs::remove_dir(fake_capture).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn saves_png_and_settings_in_a_unicode_directory() {
        let root =
            std::env::temp_dir().join(format!("截图测试 {} {}", std::process::id(), now_ms()));
        let captures = root.join("captures");
        let storage = Storage {
            root: root.clone(),
            captures: captures.clone(),
        };
        let expected = [10, 20, 30, 40, 50, 60];
        let path = storage.save_png(2, 1, &expected).unwrap();
        let mut reader = png::Decoder::new(BufReader::new(File::open(&path).unwrap()))
            .read_info()
            .unwrap();
        let mut output = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut output).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&output[..info.buffer_size()], &expected);
        assert!(is_capture(&path).unwrap());
        assert_eq!(storage.load_settings().unwrap(), Settings::default());
        let changed = Settings {
            hotkey: Hotkey {
                modifiers: MOD_ALT | MOD_SHIFT,
                key: u32::from(b'B'),
            },
            retention_minutes: 3,
        };
        storage.save_settings(Settings::default()).unwrap();
        storage.save_settings(changed).unwrap();
        assert_eq!(storage.load_settings().unwrap(), changed);
        drop(reader);
        fs::remove_file(path).unwrap();
        fs::remove_file(root.join("settings.ini")).unwrap();
        fs::remove_dir(captures).unwrap();
        fs::remove_dir(root).unwrap();
    }
}

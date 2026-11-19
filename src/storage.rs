use crate::capture::Image;
use crate::{APP_NAME, LEGACY_APP_NAME};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::windows::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt},
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, FILE_ATTRIBUTE_REPARSE_POINT, MOVEFILE_REPLACE_EXISTING,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_F1, VK_F11,
};

const CF_UNICODETEXT: u32 = 13;
const DELETE_RETRY_MS: u64 = 30_000;
const CAPTURE_TEMP: &str = "capture-tmp";
const LEGACY_HEADER_BYTES: u64 = 128;
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

#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
    pub captures: PathBuf,
}

pub struct SavedCapture {
    pub path: PathBuf,
    saved_at: u64,
}

impl SavedCapture {
    pub fn expires_in(&self, minutes: u32) -> Duration {
        Duration::from_millis(remaining_ms(self.saved_at, minutes, now_ms()))
    }
}

impl Storage {
    pub fn open() -> io::Result<Self> {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
        Self::open_in(&local_app_data)
    }

    fn open_in(local_app_data: &Path) -> io::Result<Self> {
        // Keep paths already copied to the clipboard valid when upgrading.
        let legacy_root = local_app_data.join(LEGACY_APP_NAME);
        let root = if legacy_root.try_exists()? {
            legacy_root
        } else {
            local_app_data.join(APP_NAME)
        };
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

    pub fn save_png(&self, image: &Image) -> io::Result<SavedCapture> {
        fs::create_dir_all(&self.captures)?;
        let temporary_dir = self.root.join(CAPTURE_TEMP);
        fs::create_dir_all(&temporary_dir)?;
        if !is_private_temp_dir(&temporary_dir)? {
            return Err(io::Error::other("截图临时目录必须是普通目录"));
        }
        let pid = std::process::id();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = temporary_dir.join(format!("shot-{}-{pid}-{sequence}.part", now_ms()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&temporary)?;
        let result = (|| {
            let mut buffered = BufWriter::new(file);
            write_png(&mut buffered, image)?;
            buffered.flush()?;
            drop(buffered);
            let saved_at = now_ms();
            let path = self
                .captures
                .join(format!("shot-{saved_at}-{pid}-{sequence}.png"));
            if unsafe { MoveFileExW(wide(&temporary).as_ptr(), wide(&path).as_ptr(), 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(SavedCapture { path, saved_at })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn clean(&self, retention_minutes: u32, current: impl Fn() -> bool) -> Option<u64> {
        let now = now_ms();
        let mut next = clean_at(&self.captures, retention_minutes, now, false, &current);
        if let Some(delay) = clean_at(
            &self.root.join(CAPTURE_TEMP),
            retention_minutes,
            now,
            true,
            current,
        ) {
            schedule(&mut next, delay);
        }
        next
    }
}

fn write_png(output: impl Write, image: &Image) -> io::Result<()> {
    let mut encoder = png::Encoder::new(output, image.width, image.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk("Software".to_owned(), APP_NAME.to_owned())
        .map_err(io::Error::other)?;
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    image.with_rows(|rows| {
        let mut stream = writer
            .stream_writer_with_size(64 * 1024)
            .map_err(io::Error::other)?;
        let mut rgb = vec![0; image.width as usize * 3];
        for row in rows {
            for (pixel, target) in row.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
                target.copy_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            stream.write_all(&rgb)?;
        }
        stream.finish().map_err(io::Error::other)
    })?;
    writer.finish().map_err(io::Error::other)
}

fn is_capture(path: &Path) -> io::Result<bool> {
    let file = BufReader::new(File::open(path)?);
    match png::Decoder::new(file).read_info() {
        Ok(reader) => Ok(has_software_marker(reader.info())),
        Err(png::DecodingError::IoError(error)) if error.kind() != io::ErrorKind::UnexpectedEof => {
            Err(error)
        }
        Err(_) => Ok(false),
    }
}

fn has_software_marker(info: &png::Info<'_>) -> bool {
    info.uncompressed_latin1_text.iter().any(|text| {
        text.keyword == "Software" && matches!(text.text.as_str(), APP_NAME | LEGACY_APP_NAME)
    })
}

fn is_legacy_part(path: &Path) -> io::Result<bool> {
    // The old writer put the marker immediately after IHDR, even if it was killed before IDAT.
    let mut bytes = Vec::with_capacity(LEGACY_HEADER_BYTES as usize);
    File::open(path)?
        .take(LEGACY_HEADER_BYTES)
        .read_to_end(&mut bytes)?;
    let mut input = bytes.as_slice();
    let mut decoder = png::StreamingDecoder::new();
    while !input.is_empty() {
        match decoder.update(input, None) {
            Ok((_, png::Decoded::ChunkComplete(png::chunk::tEXt)))
                if decoder.info().is_some_and(has_software_marker) =>
            {
                return Ok(true)
            }
            Ok((_, png::Decoded::ChunkBegin(_, png::chunk::IDAT | png::chunk::IEND))) | Err(_) => {
                break
            }
            Ok((count, _)) => input = &input[count..],
        }
    }
    Ok(false)
}

fn is_private_temp_dir(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0)
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

fn remaining_ms(saved_at: u64, minutes: u32, now: u64) -> u64 {
    saved_at
        .saturating_add(u64::from(minutes) * 60_000)
        .saturating_sub(now)
}

fn capture_timestamp(path: &Path, suffix: &str) -> Option<u64> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_prefix("shot-")?
        .strip_suffix(suffix)?;
    let mut fields = name.split('-');
    let timestamp = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    fields.next().is_none().then_some(timestamp)
}

fn clean_at(
    directory: &Path,
    minutes: u32,
    now: u64,
    temporary: bool,
    current: impl Fn() -> bool,
) -> Option<u64> {
    if temporary {
        match is_private_temp_dir(directory) {
            Ok(true) => (),
            Ok(false) => return None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => return Some(DELETE_RETRY_MS),
        }
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(DELETE_RETRY_MS),
    };
    let mut next = None;
    for entry in entries {
        if !current() {
            return None;
        }
        let Ok(entry) = entry else {
            schedule(&mut next, DELETE_RETRY_MS);
            continue;
        };
        let path = entry.path();
        let part = capture_timestamp(&path, ".part");
        let saved_at = if temporary {
            part
        } else {
            part.or_else(|| capture_timestamp(&path, ".png"))
        };
        let Some(saved_at) = saved_at else {
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
        let remaining = remaining_ms(saved_at, minutes, now);
        if remaining > 0 {
            schedule(&mut next, remaining);
            continue;
        }
        let marked = match (temporary, part.is_some()) {
            (true, _) => Ok(true),
            (false, true) => is_legacy_part(&path),
            (false, false) => is_capture(&path),
        };
        let result = marked.and_then(|is_capture| {
            if is_capture && current() {
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

pub enum ClipboardError {
    Busy(io::Error),
    Failed(io::Error),
}

pub fn copy_path(owner: HWND, path: &Path) -> Result<(), ClipboardError> {
    let text = wide(path);
    unsafe {
        if OpenClipboard(owner) == 0 {
            return Err(ClipboardError::Busy(io::Error::last_os_error()));
        }
        let result = set_clipboard_text(&text);
        CloseClipboard();
        result.map_err(ClipboardError::Failed)
    }
}

unsafe fn set_clipboard_text(text: &[u16]) -> io::Result<()> {
    let memory = GlobalAlloc(GMEM_MOVEABLE, size_of_val(text));
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
        if EmptyClipboard() != 0 && !SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    })();
    if result.is_err() {
        GlobalFree(memory);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::{Background, Outcome, READY};

    fn sample_image() -> Image {
        Image::from_bgra(1, 1, vec![56, 34, 12, 0])
    }
    use std::{
        ptr::{null, null_mut},
        thread,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_REMOVE,
    };

    fn write_legacy_png(path: &Path) {
        let mut encoder = png::Encoder::new(File::create(path).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder
            .add_text_chunk("Software".to_owned(), LEGACY_APP_NAME.to_owned())
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[12, 34, 56]).unwrap();
        writer.finish().unwrap();
    }

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
    fn opens_existing_data_in_place_and_uses_the_new_name_for_fresh_data() {
        let local_app_data =
            std::env::temp_dir().join(format!("TermShot-data-{}-{}", std::process::id(), now_ms()));
        let new_root = local_app_data.join(APP_NAME);
        let legacy_root = local_app_data.join(LEGACY_APP_NAME);
        let old_captures = legacy_root.join("captures");
        fs::create_dir_all(&old_captures).unwrap();
        let settings = legacy_root.join("settings.ini");
        fs::write(&settings, "modifiers=5\nkey=82\nretention_minutes=20\n").unwrap();
        let storage = Storage::open_in(&local_app_data).unwrap();
        assert_eq!(storage.captures, old_captures);
        let settings_read = storage.load_settings().unwrap();
        assert_eq!(settings_read.hotkey.key, u32::from(b'R'));
        assert_eq!(settings_read.retention_minutes, 20);
        assert!(!new_root.exists());

        fs::remove_file(settings).unwrap();
        fs::remove_dir(old_captures).unwrap();
        fs::remove_dir(legacy_root).unwrap();
        let storage = Storage::open_in(&local_app_data).unwrap();
        assert_eq!(storage.captures, new_root.join("captures"));
        assert_eq!(storage.load_settings().unwrap(), Settings::default());
        fs::remove_dir(storage.captures).unwrap();
        fs::remove_dir(new_root).unwrap();
        fs::remove_dir(local_app_data).unwrap();
    }

    #[test]
    fn cleanup_uses_original_time_and_only_generated_png_names() {
        let directory =
            std::env::temp_dir().join(format!("TermShot-test-{}-{}", std::process::id(), now_ms()));
        fs::create_dir(&directory).unwrap();
        let expired = directory.join("shot-100000-23-0.png");
        let current = directory.join("shot-100001-23-1.png");
        let unrelated = directory.join("其他图.png");
        let unmarked = directory.join("shot-100000-23-3.png");
        let fake_capture = directory.join("shot-100000-23-2.png");
        write_legacy_png(&expired);
        write_png(File::create(&current).unwrap(), &sample_image()).unwrap();
        fs::write(&unrelated, []).unwrap();
        let mut encoder = png::Encoder::new(File::create(&unmarked).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[12, 34, 56]).unwrap();
        writer.finish().unwrap();
        fs::create_dir(&fake_capture).unwrap();
        assert_eq!(
            clean_at(&directory, 2, 160_000, false, || true),
            Some(60_000)
        );
        assert_eq!(clean_at(&directory, 1, 160_000, false, || true), Some(1));
        assert!(!expired.exists());
        assert!(
            current.is_file() && unrelated.is_file() && unmarked.is_file() && fake_capture.is_dir()
        );
        assert_eq!(clean_at(&directory, 1, 160_001, false, || true), None);
        fs::remove_file(unrelated).unwrap();
        fs::remove_file(unmarked).unwrap();
        fs::remove_dir(fake_capture).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cleanup_expires_incomplete_temporary_files_and_marked_legacy_files() {
        let root = std::env::temp_dir().join(format!(
            "TermShot-parts-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let captures = root.join("captures");
        let temporary = root.join(CAPTURE_TEMP);
        fs::create_dir_all(&captures).unwrap();
        fs::create_dir(&temporary).unwrap();
        let partial = temporary.join("shot-100000-23-0.part");
        let legacy = captures.join("shot-100000-23-1.part");
        let unmarked = captures.join("shot-100000-23-2.part");
        let recent = temporary.join("shot-100001-23-3.part");
        let legacy_prefix = captures.join("shot-100000-23-4.part");
        for path in [&partial, &unmarked, &recent] {
            fs::write(path, b"unfinished").unwrap();
        }
        write_legacy_png(&legacy);
        let legacy_bytes = fs::read(&legacy).unwrap();
        let idat = legacy_bytes
            .windows(4)
            .position(|window| window == b"IDAT")
            .unwrap();
        fs::write(&legacy_prefix, &legacy_bytes[..idat - 4]).unwrap();

        let active = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&partial)
            .unwrap();
        assert_eq!(clean_at(&temporary, 1, 160_000, true, || true), Some(1));
        assert!(partial.exists());
        drop(active);
        assert_eq!(clean_at(&temporary, 1, 160_000, true, || true), Some(1));
        assert_eq!(clean_at(&captures, 1, 160_000, false, || true), None);
        assert!(!partial.exists());
        assert!(!legacy.exists());
        assert!(!legacy_prefix.exists());
        assert!(recent.exists());
        assert!(unmarked.exists());

        fs::remove_file(recent).unwrap();
        fs::remove_file(unmarked).unwrap();
        fs::remove_dir(temporary).unwrap();
        fs::remove_dir(captures).unwrap();
        fs::remove_dir(root).unwrap();
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
        let path = storage
            .save_png(&Image::from_bgra(2, 1, vec![30, 20, 10, 0, 60, 50, 40, 0]))
            .unwrap()
            .path;
        let mut reader = png::Decoder::new(BufReader::new(File::open(&path).unwrap()))
            .read_info()
            .unwrap();
        assert!(reader
            .info()
            .uncompressed_latin1_text
            .iter()
            .any(|chunk| chunk.keyword == "Software" && chunk.text == APP_NAME));
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
        fs::remove_dir(root.join(CAPTURE_TEMP)).unwrap();
        fs::remove_dir(captures).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn background_reports_a_saved_image_and_schedules_cleanup() {
        let root = std::env::temp_dir().join(format!(
            "TermShot-background-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let captures = root.join("captures");
        fs::create_dir_all(&captures).unwrap();
        let expired = captures.join("shot-100000-23-0.png");
        write_png(File::create(&expired).unwrap(), &sample_image()).unwrap();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        assert!(!hwnd.is_null());
        let mut background = Background::start(
            hwnd,
            Storage {
                root: root.clone(),
                captures: captures.clone(),
            },
            Some(1),
        )
        .unwrap();
        for _ in 0..3 {
            background.save(sample_image()).unwrap();
        }
        assert_eq!(background.save_budget(), 0);
        assert_eq!(
            background.save(sample_image()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        background.clean().unwrap();
        let mut receive = || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut message = MSG::default();
            loop {
                if unsafe { PeekMessageW(&mut message, hwnd, READY, READY, PM_REMOVE) } != 0 {
                    return background.receive().unwrap();
                }
                assert!(
                    Instant::now() < deadline,
                    "background job did not notify the window"
                );
                thread::sleep(Duration::from_millis(1));
            }
        };
        let saved = (0..3)
            .map(|_| {
                let Outcome::Saved(Ok(saved)) = receive() else {
                    panic!("expected saved PNG")
                };
                assert!(saved.expires_in(1) <= Duration::from_secs(60));
                assert!(is_capture(&saved.path).unwrap());
                saved.path
            })
            .collect::<Vec<_>>();
        assert!(matches!(receive(), Outcome::Cleaned(Some(_))));
        assert!(!background.is_saving());
        assert_eq!(background.save_budget(), usize::MAX);
        assert!(!expired.exists());
        drop(background);
        unsafe { DestroyWindow(hwnd) };
        for path in saved {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(captures).unwrap();
        fs::remove_dir(root.join(CAPTURE_TEMP)).unwrap();
        fs::remove_dir(root).unwrap();
    }
}

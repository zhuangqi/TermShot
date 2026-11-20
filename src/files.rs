use crate::{is_private_temp_dir, APP_NAME, LEGACY_APP_NAME};
use std::{
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
const DELETE_RETRY_MS: u64 = 30_000;
const LEGACY_HEADER_BYTES: u64 = 128;
pub(crate) const CAPTURE_TEMP: &str = "capture-tmp";
static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(crate) struct SavedCapture {
    pub path: std::path::PathBuf,
    saved_at: u64,
}

impl SavedCapture {
    pub fn expires_at(&self, minutes: u32) -> u64 {
        remaining_ms(self.saved_at, minutes, 0)
    }

    #[cfg(windows)]
    pub fn expires_in(&self, minutes: u32) -> std::time::Duration {
        std::time::Duration::from_millis(self.expires_at(minutes).saturating_sub(now_ms()))
    }
}

pub(crate) fn save_capture(
    root: &Path,
    captures: &Path,
    encode: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<SavedCapture> {
    let temp_dir = root.join(CAPTURE_TEMP);
    fs::create_dir_all(captures)?;
    fs::create_dir_all(&temp_dir)?;
    if !is_private_temp_dir(&temp_dir)? {
        return Err(io::Error::other("截图临时目录必须是普通目录"));
    }
    #[cfg(unix)]
    if !is_private_temp_dir(captures)? {
        return Err(io::Error::other("截图目录必须是普通目录"));
    }
    let pid = std::process::id();
    let seq = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let part = temp_dir.join(format!("shot-{}-{pid}-{seq}.part", now_ms()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
    let file = options.open(&part)?;
    let result = (|| {
        let mut writer = io::BufWriter::new(file);
        encode(&mut writer)?;
        writer.flush()?;
        drop(writer);
        let saved_at = now_ms();
        let path = captures.join(format!("shot-{saved_at}-{pid}-{seq}.png"));
        // Publish the completed file without replacing an existing capture.
        #[cfg(windows)]
        {
            use crate::storage::wide;
            use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
            if unsafe { MoveFileExW(wide(&part).as_ptr(), wide(&path).as_ptr(), 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(unix)]
        fs::hard_link(&part, &path)?;
        Ok(SavedCapture { path, saved_at })
    })();
    if cfg!(unix) || result.is_err() {
        let _ = fs::remove_file(part);
    }
    result
}

pub(crate) fn save_budget(jobs: usize, bytes: usize) -> usize {
    match jobs {
        0 => usize::MAX,
        1 | 2 => (64 * 1024 * 1024usize).saturating_sub(bytes),
        _ => 0,
    }
}

pub(crate) fn write_png(
    output: impl Write,
    width: u32,
    height: u32,
    rows: &mut dyn Iterator<Item = &[u8]>,
) -> io::Result<()> {
    let mut encoder = png::Encoder::new(output, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk("Software".to_owned(), APP_NAME.to_owned())
        .map_err(io::Error::other)?;
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    {
        let mut stream = writer
            .stream_writer_with_size(64 * 1024)
            .map_err(io::Error::other)?;
        let mut rgb = vec![0; width as usize * 3];
        for row in rows {
            for (pixel, target) in row.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
                target.copy_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            stream.write_all(&rgb)?;
        }
        stream.finish().map_err(io::Error::other)
    }?;
    writer.finish().map_err(io::Error::other)
}

pub(crate) fn is_capture(path: &Path) -> io::Result<bool> {
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

pub(crate) fn is_legacy_part(path: &Path) -> io::Result<bool> {
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

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn remaining_ms(saved_at: u64, minutes: u32, now: u64) -> u64 {
    saved_at
        .saturating_add(u64::from(minutes) * 60_000)
        .saturating_sub(now)
}

pub(crate) fn capture_timestamp(path: &Path, suffix: &str) -> Option<u64> {
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

pub(crate) fn clean_captures(
    root: &Path,
    captures: &Path,
    minutes: u32,
    current: impl Fn() -> bool,
) -> Option<u64> {
    let now = now_ms();
    let mut next = clean_at(captures, minutes, now, false, &current);
    if let Some(delay) = clean_at(&root.join(CAPTURE_TEMP), minutes, now, true, current) {
        schedule(&mut next, delay);
    }
    next
}

pub(crate) fn clean_at(
    directory: &Path,
    minutes: u32,
    now: u64,
    temporary: bool,
    current: impl Fn() -> bool,
) -> Option<u64> {
    if temporary || cfg!(unix) {
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

pub(crate) fn schedule(next: &mut Option<u64>, delay: u64) {
    *next = Some(next.map_or(delay, |current| current.min(delay)));
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_limits_concurrent_bytes_and_jobs() {
        let limit = 64 * 1024 * 1024;
        assert_eq!(save_budget(0, 0), usize::MAX);
        assert_eq!(save_budget(1, limit - 1), 1);
        assert_eq!(save_budget(1, limit + 1), 0);
        assert_eq!(save_budget(2, 1024), limit - 1024);
        assert_eq!(save_budget(3, 1024), 0);
    }

    #[test]
    fn failed_encode_discards_partial_file_and_allows_retry() {
        let root = std::env::temp_dir().join(format!(
            "termshot-write-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let captures = root.join("captures");
        assert!(save_capture(&root, &captures, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        })
        .is_err());
        assert_eq!(fs::read_dir(&captures).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.join(CAPTURE_TEMP)).unwrap().count(), 0);
        let saved = save_capture(&root, &captures, |file| file.write_all(b"complete")).unwrap();
        assert_eq!(fs::read(saved.path).unwrap(), b"complete");
        fs::remove_dir_all(root).unwrap();
    }
}

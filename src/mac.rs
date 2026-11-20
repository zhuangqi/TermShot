use crate::files::{clean_captures, save_capture, write_png, SavedCapture};
use std::{
    ffi::{c_char, CStr, CString, OsStr},
    fs, io,
    os::unix::ffi::OsStrExt,
    path::Path,
};

extern "C" {
    fn termshot_app();
}

pub fn run() {
    unsafe { termshot_app() }
}

pub(crate) fn is_private_temp_dir(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.is_dir())
}

const RETENTION_MINUTES: u32 = 10;

fn save(
    root: &Path,
    width: u32,
    height: u32,
    stride: usize,
    pixels: &[u8],
) -> io::Result<SavedCapture> {
    let row_bytes = width as usize * 4;
    let required = stride
        .checked_mul(height.saturating_sub(1) as usize)
        .and_then(|n| n.checked_add(row_bytes));
    if width == 0 || height == 0 || stride < row_bytes || required.is_none_or(|n| n > pixels.len())
    {
        return Err(io::Error::other("无效的截图行跨度或缓冲区"));
    }
    save_capture(root, &root.join("captures"), |output| {
        let mut rows = pixels
            .chunks(stride)
            .take(height as usize)
            .map(|row| &row[..row_bytes]);
        write_png(output, width, height, &mut rows)
    })
}

#[repr(C)]
struct SaveResult {
    text: *mut c_char,
    expires_ms: u64,
}

// The caller holds a read lock or owns the bitmap until encoding completes.
// expires_ms is zero on error. text always belongs to Rust.
#[no_mangle]
unsafe extern "C" fn termshot_save(
    root: *const c_char,
    pixels: *const u8,
    length: usize,
    width: u32,
    height: u32,
    stride: usize,
) -> SaveResult {
    let result = if root.is_null() || pixels.is_null() || length > isize::MAX as usize {
        Err(io::Error::other("无效的截图缓冲区"))
    } else {
        let root = Path::new(OsStr::from_bytes(CStr::from_ptr(root).to_bytes()));
        save(
            root,
            width,
            height,
            stride,
            std::slice::from_raw_parts(pixels, length),
        )
    };
    let (text, expires_ms) = match result {
        Ok(saved) => (
            CString::new(saved.path.as_os_str().as_bytes()).unwrap(),
            saved.expires_at(RETENTION_MINUTES),
        ),
        Err(error) => (
            CString::new(error.to_string().replace('\0', " ")).unwrap(),
            0,
        ),
    };
    SaveResult {
        text: text.into_raw(),
        expires_ms,
    }
}

#[no_mangle]
extern "C" fn termshot_budget(jobs: usize, bytes: usize) -> usize {
    crate::files::save_budget(jobs, bytes)
}

#[no_mangle]
unsafe extern "C" fn termshot_free(path: *mut c_char) {
    drop(CString::from_raw(path));
}

#[no_mangle]
unsafe extern "C" fn termshot_clean(root: *const c_char) -> u64 {
    let root = Path::new(OsStr::from_bytes(CStr::from_ptr(root).to_bytes()));
    clean_captures(root, &root.join("captures"), RETENTION_MINUTES, || true).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{clean_at, now_ms};

    #[test]
    fn saved_crop_uses_shared_encoder_and_retention_rules() {
        let root =
            std::env::temp_dir().join(format!("termshot-test-{}-{}", std::process::id(), now_ms()));
        let dir = root.join("captures");
        let path = save(&root, 1, 2, 8, &[1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 255]).unwrap();
        let path = path.path.as_path();
        let file = std::io::BufReader::new(fs::File::open(path).unwrap());
        let mut decoder = png::Decoder::new(file).read_info().unwrap();
        let mut rgb = [0; 6];
        decoder.next_frame(&mut rgb).unwrap();
        assert_eq!(rgb, [3, 2, 1, 6, 5, 4]);
        let stamp = crate::files::capture_timestamp(path, ".png").unwrap();
        assert_eq!(
            clean_at(&dir, RETENTION_MINUTES, stamp, false, || true),
            Some(600_000)
        );
        assert_eq!(
            clean_at(&dir, RETENTION_MINUTES, stamp + 600_000, false, || true),
            None
        );
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

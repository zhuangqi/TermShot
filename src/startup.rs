use crate::{storage::wide, LEGACY_APP_NAME};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_NOEXPAND,
    RRF_RT_ANY,
};

pub const ARG: &str = "--startup";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const MAX_COMMAND_LENGTH: usize = 260;

pub fn is_enabled() -> io::Result<bool> {
    Ok(Entry::read(RUN_KEY)?.0.is_some())
}

pub fn update_legacy_executable() -> io::Result<()> {
    update_legacy_executable_in(RUN_KEY, std::env::current_exe)
}

fn update_legacy_executable_in(
    subkey: &str,
    current_exe: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<()> {
    let Entry(Some((REG_SZ, data))) = Entry::read(subkey)? else {
        return Ok(());
    };
    let utf16: Vec<_> = data
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    let command = String::from_utf16_lossy(&utf16);
    let old_exe = command
        .trim_end_matches('\0')
        .strip_prefix('"')
        .and_then(|command| command.split_once('"'))
        .filter(|(_, args)| args.trim() == ARG)
        .and_then(|(path, _)| Path::new(path).file_name());
    if old_exe.is_some_and(|name| name.eq_ignore_ascii_case("CliScreenshot.exe")) {
        Entry(Some((REG_SZ, command_data(&current_exe()?)?))).write(subkey)?;
    }
    Ok(())
}

pub fn set_enabled(enabled: bool) -> io::Result<Entry> {
    let next = if enabled {
        Entry(Some((REG_SZ, command_data(&std::env::current_exe()?)?)))
    } else {
        Entry(None)
    };
    let previous = Entry::read(RUN_KEY)?;
    next.write(RUN_KEY)?;
    Ok(previous)
}

pub struct Entry(Option<(u32, Vec<u8>)>);

impl Entry {
    pub fn restore(&self) -> io::Result<()> {
        self.write(RUN_KEY)
    }

    fn read(subkey: &str) -> io::Result<Self> {
        let subkey = wide(subkey);
        // Preserve the Run value name so Windows retains its startup approval setting.
        let name = wide(LEGACY_APP_NAME);
        let mut kind = 0;
        let mut data = vec![0; 2 * (MAX_COMMAND_LENGTH + 1)];
        loop {
            let mut size = data.len() as u32;
            let status = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    RRF_RT_ANY | RRF_NOEXPAND,
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            match status {
                ERROR_FILE_NOT_FOUND => return Ok(Self(None)),
                ERROR_MORE_DATA => data.resize((size as usize).max(data.len() + 4), 0),
                ERROR_SUCCESS => {
                    data.truncate(size as usize);
                    return Ok(Self(Some((kind, data))));
                }
                error => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
    }

    fn write(&self, subkey: &str) -> io::Result<()> {
        let subkey = wide(subkey);
        let name = wide(LEGACY_APP_NAME);
        let status = unsafe {
            if let Some((kind, data)) = &self.0 {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    *kind,
                    data.as_ptr().cast(),
                    data.len() as u32,
                )
            } else {
                let status = RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr());
                if status == ERROR_FILE_NOT_FOUND {
                    ERROR_SUCCESS
                } else {
                    status
                }
            }
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }
}

fn command_data(executable: &Path) -> io::Result<Vec<u8>> {
    let mut command = OsString::from("\"");
    command.push(executable);
    command.push("\" ");
    command.push(ARG);
    let command = wide(command);
    if command.len() > MAX_COMMAND_LENGTH + 1 {
        return Err(io::Error::other("程序路径太长，无法注册开机启动"));
    }
    Ok(command.into_iter().flat_map(u16::to_le_bytes).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;

    struct TestKey(String);

    impl Drop for TestKey {
        fn drop(&mut self) {
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(&self.0).as_ptr()) };
        }
    }

    #[test]
    fn startup_entry_round_trips_a_quoted_unicode_path_and_can_be_removed() {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let subkey = TestKey(format!(
            "Software\\TermShot-test-{}-{}",
            std::process::id(),
            timestamp.as_nanos()
        ));
        let previous = Entry::read(&subkey.0).unwrap();
        assert!(previous.0.is_none());
        let path = Path::new(r"C:\截图 工具\TermShot.exe");
        let enabled = Entry(Some((REG_SZ, command_data(path).unwrap())));
        enabled.write(&subkey.0).unwrap();

        let (kind, data) = Entry::read(&subkey.0).unwrap().0.unwrap();
        let command: Vec<_> = data
            .chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .collect();
        assert_eq!(kind, REG_SZ);
        assert_eq!(command, wide(format!("\"{}\" --startup", path.display())));

        previous.write(&subkey.0).unwrap();
        assert!(Entry::read(&subkey.0).unwrap().0.is_none());
    }

    #[test]
    fn updates_the_legacy_startup_path_once_and_keeps_disabled_startup_off() {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let subkey = TestKey(format!(
            "Software\\TermShot-upgrade-{}-{}",
            std::process::id(),
            timestamp.as_nanos()
        ));
        update_legacy_executable_in(&subkey.0, || panic!("startup is disabled")).unwrap();
        assert!(Entry::read(&subkey.0).unwrap().0.is_none());

        let old = command_data(Path::new(r"C:\截图 工具\CliScreenshot.exe")).unwrap();
        Entry(Some((REG_SZ, old))).write(&subkey.0).unwrap();
        let new = Path::new(r"D:\新目录\TermShot.exe");
        update_legacy_executable_in(&subkey.0, || Ok(new.to_path_buf())).unwrap();
        assert_eq!(
            Entry::read(&subkey.0).unwrap().0,
            Some((REG_SZ, command_data(new).unwrap()))
        );
        update_legacy_executable_in(&subkey.0, || panic!("startup is already updated")).unwrap();
    }
}

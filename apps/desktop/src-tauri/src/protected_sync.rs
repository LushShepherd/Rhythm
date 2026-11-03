//! Windows-user-bound storage for OAuth tokens and the unlocked vault key.
//! This file is deliberately separate from journal backups and exports.
use std::{fs, io::Write, path::Path};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::{PCWSTR, w},
};
use zeroize::Zeroizing;

const LIMIT: u64 = 32 * 1024 * 1024;

fn transform(bytes: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>, String> {
    if bytes.len() as u64 > LIMIT {
        return Err("The local sync connection is too large to save safely.".into());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: DPAPI reads the live input buffer and allocates output with LocalAlloc.
    // No UI or machine-wide flag is used: protection belongs to the current Windows user.
    let result = unsafe {
        if protect {
            CryptProtectData(
                &input,
                w!("Rhythm Google Drive connection"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    result.map_err(|_| "Windows could not unlock or protect the saved sync connection. Local notes are unchanged.".to_string())?;
    if output.pbData.is_null() {
        return Err("Windows returned an empty protected connection.".into());
    }
    // SAFETY: successful DPAPI provides cbData initialized bytes; copy before freeing.
    let value = unsafe {
        let buffer = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let value = Zeroizing::new(buffer.to_vec());
        use zeroize::Zeroize;
        buffer.zeroize();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        value
    };
    Ok(value)
}

pub fn read(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let metadata = match fs::metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("The saved sync connection could not be read.".into()),
    };
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err("The saved sync connection is invalid.".into());
    }
    let bytes = fs::read(path).map_err(|_| "The saved sync connection could not be read.")?;
    transform(&bytes, false).map(Some)
}

pub fn write(path: &Path, plaintext: &[u8]) -> Result<(), String> {
    let protected = transform(plaintext, true)?;
    let parent = path.parent().ok_or("Missing sync connection directory.")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "The sync connection could not be saved. Check free disk space.")?;
    file.write_all(&protected)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| "The sync connection could not be saved. Check free disk space.")?;
    file.persist(path)
        .map_err(|_| "The sync connection could not be replaced safely.")?;
    Ok(())
}

pub fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("The saved connection could not be removed. Try disconnecting again.".into()),
    }
}

pub fn open_google_authorization(url: &str) -> Result<(), String> {
    // The OAuth module constructs this URL. Never expose a general shell opener to JS.
    if !url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?") {
        return Err("The Google sign-in address is invalid.".into());
    }
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the UTF-16 URL is NUL terminated and alive throughout ShellExecuteW.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("Windows could not open your browser for Google sign-in.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_protected_and_replacement_survives_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sync.protected");
        write(&path, b"private refresh token and vault key").unwrap();
        let disk = fs::read(&path).unwrap();
        assert!(!disk.windows(7).any(|bytes| bytes == b"private"));
        assert_eq!(
            &**read(&path).unwrap().unwrap(),
            b"private refresh token and vault key"
        );
        write(&path, b"updated token").unwrap();
        assert_eq!(&**read(&path).unwrap().unwrap(), b"updated token");
        remove(&path).unwrap();
        assert!(read(&path).unwrap().is_none());
    }

    #[test]
    fn corrupt_credentials_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sync.protected");
        fs::write(&path, b"not a protected connection").unwrap();
        assert!(read(&path).is_err());
    }
}

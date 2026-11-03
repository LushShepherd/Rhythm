//! Native user-selected destinations. No web-provided path reaches these operations.
use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_DONTADDTORECENT,
            OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
    },
    core::{PCWSTR, PWSTR},
};

#[derive(Clone, Copy)]
pub enum Operation {
    Backup,
    Export,
    EncryptedNotes,
    Restore,
}

static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);
struct DialogGuard;
impl Drop for DialogGuard {
    fn drop(&mut self) {
        DIALOG_OPEN.store(false, Ordering::Release);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn title(operation: Operation, language: &str) -> &'static str {
    match (operation, language) {
        (Operation::Backup, "zh-Hans") => "保存 Rhythm 备份（新文件）",
        (Operation::Backup, "zh-Hant") => "儲存 Rhythm 備份（新檔案）",
        (Operation::Backup, _) => "Save Rhythm backup (new file)",
        (Operation::Export, "zh-Hans") => "导出 Markdown（新文件）",
        (Operation::Export, "zh-Hant") => "匯出 Markdown（新檔案）",
        (Operation::Export, _) => "Export Markdown (new file)",
        (Operation::EncryptedNotes, "zh-Hans") => "保存加密随记（新文件）",
        (Operation::EncryptedNotes, "zh-Hant") => "儲存加密隨記（新檔案）",
        (Operation::EncryptedNotes, _) => "Save encrypted notes (new file)",
        (Operation::Restore, "zh-Hans") => "选择要恢复的 Rhythm 备份",
        (Operation::Restore, "zh-Hant") => "選擇要還原的 Rhythm 備份",
        (Operation::Restore, _) => "Choose a Rhythm backup to restore",
    }
}

/// Run on a blocking thread while the timer worker continues independently.
/// `owner` comes only from Tauri's native main-window handle.
pub fn choose(
    operation: Operation,
    owner: usize,
    language: &str,
) -> Result<Option<PathBuf>, String> {
    if DIALOG_OPEN.swap(true, Ordering::AcqRel) {
        return Err("Finish the open file dialog first.".into());
    }
    let _guard = DialogGuard;
    let restoring = matches!(operation, Operation::Restore);
    let (extension, prefix, filter) = match operation {
        Operation::Export => ("md", "journal", "Markdown (*.md)\0*.md\0\0"),
        Operation::EncryptedNotes => (
            "rhythm",
            "notes",
            "Encrypted Rhythm notes (*.rhythm)\0*.rhythm\0\0",
        ),
        _ => (
            "sqlite3",
            "backup",
            "Rhythm backup (*.sqlite3)\0*.sqlite3\0\0",
        ),
    };
    let filter = wide(filter);
    let caption = wide(title(operation, language));
    let extension_wide = wide(extension);
    // Windows' Unicode path limit, including the final NUL.
    let mut filename = vec![0u16; 32_768];
    if !restoring {
        let suggested = wide(&format!(
            "rhythm-{}-{}.{}",
            prefix,
            uuid::Uuid::new_v4(),
            extension
        ));
        filename[..suggested.len()].copy_from_slice(&suggested);
    }
    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut _),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrTitle: PCWSTR(caption.as_ptr()),
        lpstrDefExt: PCWSTR(extension_wide.as_ptr()),
        Flags: OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    if restoring {
        options.Flags |= OFN_FILEMUSTEXIST;
    }
    // SAFETY: All buffers remain alive and correctly sized throughout this synchronous
    // call. No hooks/callbacks or arbitrary pointers are provided. The main window
    // remains owned by Tauri; this adapter neither destroys nor retains its handle.
    let selected = unsafe {
        if restoring {
            GetOpenFileNameW(&mut options)
        } else {
            GetSaveFileNameW(&mut options)
        }
    };
    if !selected.as_bool() {
        // SAFETY: Reads the common-dialog error for the calling thread; no pointers.
        let error = unsafe { CommDlgExtendedError() };
        return if error.0 == 0 {
            Ok(None)
        } else {
            Err(format!(
                "Windows could not open the file dialog ({:#x}).",
                error.0
            ))
        };
    }
    let length = filename
        .iter()
        .position(|unit| *unit == 0)
        .ok_or("Windows returned an invalid filename.")?;
    let path = PathBuf::from(OsString::from_wide(&filename[..length]));
    Ok(Some(path))
}

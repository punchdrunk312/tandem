//! Dropping files into the program under the cursor, as if they had been dragged there:
//! a real drag and drop that starts and ends at the cursor.

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::core::{implement, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::S_OK;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
};
use windows::Win32::UI::Shell::{
    BHID_DataObject, ILCreateFromPathW, ILFree, IShellItemArray,
    SHCreateShellItemArrayFromIDLists,
};

const DRAGDROP_S_DROP: HRESULT = HRESULT(0x0004_0100);
const DRAGDROP_S_CANCEL: HRESULT = HRESULT(0x0004_0101);
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x0004_0102);

/// Lets the program see the files arrive for a few rounds, then lets go.
#[implement(IDropSource)]
struct Source {
    rounds: AtomicU32,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, esc: BOOL, _keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if esc.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if self.rounds.fetch_add(1, Ordering::Relaxed) < 4 {
            S_OK
        } else {
            DRAGDROP_S_DROP
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drops `paths` on whatever is under the cursor. True if the program took them.
pub fn drop_here(paths: Vec<PathBuf>) -> bool {
    let t = std::thread::Builder::new()
        .name("tandem-drop".into())
        .spawn(move || unsafe {
            if OleInitialize(None).is_err() {
                return false;
            }
            let ok = run(&paths).unwrap_or_else(|e| {
                log::info!("drop: couldn't hand the files over: {e}");
                false
            });
            OleUninitialize();
            ok
        });
    t.ok().and_then(|t| t.join().ok()).unwrap_or(false)
}

unsafe fn run(paths: &[PathBuf]) -> windows::core::Result<bool> {
    let wide: Vec<Vec<u16>> = paths
        .iter()
        .map(|p| p.as_os_str().encode_wide().chain(Some(0)).collect())
        .collect();
    let pidls: Vec<_> = wide
        .iter()
        .map(|w| ILCreateFromPathW(PCWSTR(w.as_ptr())))
        .filter(|p| !p.is_null())
        .collect();
    if pidls.is_empty() {
        return Ok(false);
    }
    let consts: Vec<_> = pidls.iter().map(|p| *p as *const _).collect();
    let data: windows::core::Result<IDataObject> = SHCreateShellItemArrayFromIDLists(&consts)
        .and_then(|a: IShellItemArray| a.BindToHandler(None, &BHID_DataObject));
    for p in &pidls {
        ILFree(Some(*p));
    }
    let data = data?;
    // The drag only moves on when something happens: a "moved by nothing" now and
    // then, so the cursor stays put.
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    std::thread::spawn(move || {
        for _ in 0..120 {
            if d2.load(Ordering::Relaxed) {
                break;
            }
            let i = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: MOUSEEVENTF_MOVE,
                        ..Default::default()
                    },
                },
            };
            SendInput(&[i], std::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let source: IDropSource = Source {
        rounds: AtomicU32::new(0),
    }
    .into();
    let mut effect = DROPEFFECT_NONE;
    let hr = DoDragDrop(
        &data,
        &source,
        DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
        &mut effect,
    );
    done.store(true, Ordering::Relaxed);
    Ok(hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE)
}

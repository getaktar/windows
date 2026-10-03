//! File Explorer's right-click menu in the Microsoft Store (MSIX) package:
//! "Upload with Aktar" and "Watch with Aktar". A package can't add the
//! registry entries the NSIS install uses (see `shell`), so the manifest
//! declares the menu entries (`windows.fileExplorerContextMenus`) and a COM
//! server, which is aktar.exe started with `--explorer-command`. That
//! process only serves Explorer: it hands what was clicked to Aktar as
//! `--upload <paths>` or `--watch <folder>`, the same as the NSIS menu, and
//! exits once Explorer lets go of it. On Windows 11 the entries show in the
//! new menu, not just under "Show more options".

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, BOOL, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_NOINTERFACE, E_NOTIMPL};
use windows::Win32::System::Com::{
    CoInitializeEx, CoRegisterClassObject, CoRevokeClassObject, CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl,
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, REGCLS_MULTIPLEUSE,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW, ECF_DEFAULT, ECS_ENABLED, SIGDN_FILESYSPATH,
};

/// What the manifest starts the COM server with.
pub const SERVER_FLAG: &str = "--explorer-command";

/// The classes in packaging/msix/AppxManifest.xml.
const UPLOAD_CLSID: GUID = GUID::from_u128(0x6c2f6a5e_5b1d_4f0e_9a57_3d1c2b7a8e41);
const WATCH_CLSID: GUID = GUID::from_u128(0x0b8e4d73_92a6_4c55_8f1e_7a4d6c2e9b13);

/// Commands Explorer holds right now; the server exits once there are none
/// for a while.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static ACTIVITY: Mutex<Option<Sender<()>>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Upload,
    Watch,
}

impl Verb {
    fn title(self) -> String {
        match self {
            Verb::Upload => crate::t!("Upload with Aktar"),
            Verb::Watch => crate::t!("Watch with Aktar"),
        }
    }

    fn clsid(self) -> GUID {
        match self {
            Verb::Upload => UPLOAD_CLSID,
            Verb::Watch => WATCH_CLSID,
        }
    }
}

fn touch() {
    if let Some(sender) = ACTIVITY.lock().unwrap().as_ref() {
        let _ = sender.send(());
    }
}

#[implement(IExplorerCommand)]
struct Command {
    verb: Verb,
}

impl Command {
    fn new(verb: Verb) -> Self {
        LIVE.fetch_add(1, Ordering::SeqCst);
        touch();
        Self { verb }
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        touch();
    }
}

fn duplicate(text: &str) -> windows::core::Result<PWSTR> {
    unsafe { SHStrDupW(&HSTRING::from(text)) }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        duplicate(&self.verb.title())
    }

    fn GetIcon(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        duplicate(&format!("{},0", exe.display()))
    }

    fn GetToolTip(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(self.verb.clsid())
    }

    fn GetState(&self, _items: Ref<'_, IShellItemArray>, _ok_to_be_slow: BOOL) -> windows::core::Result<u32> {
        Ok(ECS_ENABLED.0 as u32)
    }

    fn Invoke(&self, items: Ref<'_, IShellItemArray>, _context: Ref<'_, IBindCtx>) -> windows::core::Result<()> {
        touch();
        let items = items.ok()?;
        let mut paths = Vec::new();
        for index in 0..unsafe { items.GetCount()? } {
            // Something without a file system path (a library, a phone's
            // folder) is left out, not the whole selection with it.
            let Ok(item) = (unsafe { items.GetItemAt(index) }) else { continue };
            let Ok(name) = (unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }) else { continue };
            if let Ok(path) = unsafe { name.to_string() } {
                paths.push(path);
            }
            unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
        }
        let flag = match self.verb {
            Verb::Upload => crate::shell::UPLOAD_FLAG,
            // One folder at a time: the first one picked.
            Verb::Watch => {
                paths.truncate(1);
                crate::shell::WATCH_FLAG
            }
        };
        if paths.is_empty() {
            return Ok(());
        }
        // A new aktar.exe, which hands the paths to the running copy (or
        // becomes it), exactly as the NSIS install's menu does.
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        let _ = std::process::Command::new(exe).arg(flag).args(paths).spawn();
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory)]
struct Factory {
    verb: Verb,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, windows::core::IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if object.is_null() {
            return Err(E_NOINTERFACE.into());
        }
        unsafe { *object = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let command: IExplorerCommand = Command::new(self.verb).into();
        unsafe { command.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        touch();
        Ok(())
    }
}

/// Serves Explorer until it's done with the menu: no command held for a
/// few seconds, or ten minutes at most.
pub fn serve() {
    crate::i18n::apply(language_setting().as_deref());
    let (sender, receiver) = channel();
    *ACTIVITY.lock().unwrap() = Some(sender);
    let registered = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        [Verb::Upload, Verb::Watch]
            .into_iter()
            .filter_map(|verb| {
                let factory: IClassFactory = Factory { verb }.into();
                CoRegisterClassObject(&verb.clsid(), &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE).ok()
            })
            .collect::<Vec<u32>>()
    };
    let started = Instant::now();
    let mut idle_since = Instant::now();
    while started.elapsed() < Duration::from_secs(600) {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(()) => idle_since = Instant::now(),
            Err(_) if LIVE.load(Ordering::SeqCst) == 0 && idle_since.elapsed() > Duration::from_secs(5) => break,
            Err(_) => {}
        }
    }
    for cookie in registered {
        let _ = unsafe { CoRevokeClassObject(cookie) };
    }
}

/// The language picked in Settings: this process doesn't load the app's
/// settings otherwise.
fn language_setting() -> Option<String> {
    let path = std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("com.getaktar.windows").join("settings.json");
    let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    settings.get("language")?.as_str().map(str::to_string)
}

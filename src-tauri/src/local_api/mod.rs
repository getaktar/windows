//! The local API that companion tools (the Raycast extension) talk to. It's
//! off until the user turns it on in Settings > Integrations or approves a
//! connection request, listens on 127.0.0.1 only, and every request needs
//! the token, which lives in Credential Manager. The protocol is the same
//! as the Mac app's, so the same extension works with both.

mod router;
mod server;

pub(crate) use router::staged_name;

use std::sync::Mutex;

use rand::RngCore;
use serde::Serialize;
use tauri::async_runtime::JoinHandle;

use crate::core::{events, SharedCore};
use crate::credentials;

pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Status {
    Off,
    Starting,
    Running,
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalApiState {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
    pub status: Status,
}

pub struct LocalApiService {
    status: Mutex<Status>,
    token: Mutex<String>,
    server: Mutex<Option<JoinHandle<()>>>,
}

impl Default for LocalApiService {
    fn default() -> Self {
        Self { status: Mutex::new(Status::Off), token: Mutex::new(String::new()), server: Mutex::new(None) }
    }
}

impl LocalApiService {
    pub fn token(&self) -> String {
        self.token.lock().unwrap().clone()
    }
}

pub fn state(core: &SharedCore) -> LocalApiState {
    let settings = core.settings.get();
    LocalApiState {
        enabled: settings.local_api_enabled,
        port: settings.local_api_port,
        token: core.local_api.token(),
        status: core.local_api.status.lock().unwrap().clone(),
    }
}

pub fn start(core: &SharedCore) {
    *core.local_api.token.lock().unwrap() = credentials::load_api_token().unwrap_or_default();
    restart(core);
}

pub fn set_enabled(core: &SharedCore, enabled: bool) {
    if core.settings.get().local_api_enabled == enabled {
        return;
    }
    core.settings.update(|settings| settings.local_api_enabled = enabled);
    restart(core);
}

pub fn set_port(core: &SharedCore, port: u16) -> Result<(), String> {
    if port < 1024 {
        return Err(crate::t!("The port must be a number between 1024 and 65535."));
    }
    if core.settings.get().local_api_port != port {
        core.settings.update(|settings| settings.local_api_port = port);
        restart(core);
    }
    Ok(())
}

/// Replaces the token; anything already connected has to reconnect.
pub fn regenerate_token(core: &SharedCore) {
    let token = generate_token();
    credentials::save_api_token(&token);
    *core.local_api.token.lock().unwrap() = token;
    restart(core);
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn set_status(core: &SharedCore, status: Status) {
    *core.local_api.status.lock().unwrap() = status;
    core.notify(events::LOCAL_API_CHANGED);
}

fn restart(core: &SharedCore) {
    if let Some(server) = core.local_api.server.lock().unwrap().take() {
        server.abort();
    }
    let settings = core.settings.get();
    if !settings.local_api_enabled {
        set_status(core, Status::Off);
        return;
    }
    let token = {
        let mut token = core.local_api.token.lock().unwrap();
        if token.is_empty() {
            *token = generate_token();
            credentials::save_api_token(&token);
        }
        token.clone()
    };
    set_status(core, Status::Starting);

    let port = settings.local_api_port;
    let server_core = core.clone();
    let handle = tauri::async_runtime::spawn(async move {
        // The previous listener is dropped when its task is aborted; give
        // the socket a moment to be released before binding the same port.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        match listen(port) {
            Ok(listener) => {
                set_status(&server_core, Status::Running);
                server::serve(listener, port, token, server_core).await;
            }
            Err(error) => set_status(&server_core, Status::Failed { message: bind_error_message(&error) }),
        }
    });
    *core.local_api.server.lock().unwrap() = Some(handle);
}

/// Binds 127.0.0.1:`port` for this process alone. Windows otherwise lets
/// another program bind the same address on top (SO_REUSEADDR), which could
/// then receive the requests, bearer token included.
fn listen(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{setsockopt, SOL_SOCKET, SO_EXCLUSIVEADDRUSE};
        let enabled: i32 = 1;
        let result = unsafe {
            setsockopt(
                socket.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                (&enabled as *const i32).cast(),
                std::mem::size_of::<i32>() as i32,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    socket.bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))?;
    socket.listen(128)
}

fn bind_error_message(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        // WSAEACCES: Hyper-V, WSL, or Docker reserved a range of ports that
        // includes this one ("netsh int ipv4 show excludedportrange").
        Some(10013) => crate::t!("Windows has reserved this port for another feature. Pick a different port."),
        // WSAEADDRINUSE
        Some(10048) => crate::t!("Another app is already using this port. Pick a different port."),
        _ => error.to_string(),
    }
}

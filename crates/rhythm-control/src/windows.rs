//! Audited Win32 security boundary; transport I/O and cancellation use Tokio.
use super::*;
use std::{ffi::c_void, ptr, thread, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    runtime::Builder,
    sync::oneshot,
    time::{sleep, timeout},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub struct ServerGuard {
    shutdown: Option<oneshot::Sender<()>>,
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

pub fn start_server(
    directory: &Path,
    handler: impl Fn(ControlRequest) -> ControlResponse + Send + 'static,
) -> io::Result<ServerGuard> {
    start_server_with_completion(directory, handler, |_| {})
}

/// Complete application side effects that must follow the response attempt, such
/// as exiting after a confirmed quit. The callback runs after the client consumes
/// the reply or the bounded write/acknowledgment attempt ends, including disconnects.
/// It also receives rejected and replayed responses; callers must match their own
/// successful pending operation before taking action. It must not block the server.
pub fn start_server_with_completion(
    directory: &Path,
    handler: impl Fn(ControlRequest) -> ControlResponse + Send + 'static,
    completion: impl Fn(&ControlResponse) + Send + 'static,
) -> io::Result<ServerGuard> {
    let sid = user_sid()?;
    let name = pipe_name(directory, &sid)?;
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let first = {
        let _enter = runtime.enter();
        create_pipe(&name, &sid, true)?
    };
    let (shutdown, mut stopping) = oneshot::channel();
    thread::Builder::new()
        .name("rhythm-control".into())
        .spawn(move || {
            runtime.block_on(async move {
                let serve = async {
                    let mut pending = first;
                    let mut cache = ReplayCache::default();
                    loop {
                        let accepted = pending.connect().await;
                        // Keep an owned listener present so the pipe name cannot
                        // be claimed in a gap between client connections.
                        let next = match create_pipe(&name, &sid, false) {
                            Ok(next) => next,
                            Err(_) => break,
                        };
                        let mut connected = std::mem::replace(&mut pending, next);
                        if let Err(error) = accepted {
                            // A client can open, write, and disappear before accept is
                            // polled. Re-arm that instance instead of losing the listener.
                            if matches!(error.raw_os_error(), Some(109 | 232 | 233)) {
                                continue;
                            }
                            break;
                        }
                        let request =
                            timeout(Duration::from_secs(5), read_request(&mut connected)).await;
                        if let Ok(Ok(request)) = request {
                            let response = cache.handle(request, &handler);
                            let written = timeout(
                                Duration::from_secs(5),
                                write_response(&mut connected, &response),
                            )
                            .await;
                            if matches!(written, Ok(Ok(()))) {
                                // DisconnectNamedPipe discards unread data. A bounded
                                // acknowledgment ensures the client consumed the reply.
                                let _ = timeout(Duration::from_secs(5), connected.read_u8()).await;
                            }
                            // A completed save-and-quit must not exit before the
                            // reply is consumed, nor stay stuck open when its client
                            // disappears after the application committed the action.
                            completion(&response);
                        }
                        // Abandoned clients consume only bounded time and one slot;
                        // neither the timer worker nor unbounded threads are involved.
                    }
                };
                tokio::select! { _ = serve => {}, _ = &mut stopping => {} }
            });
        })?;
    Ok(ServerGuard {
        shutdown: Some(shutdown),
    })
}

pub fn send(directory: &Path, request: &ControlRequest) -> Result<ControlResponse, ControlError> {
    request
        .validate()
        .map_err(|message| ControlError::new("invalid_request", message))?;
    let bytes = serde_json::to_vec(request)
        .map_err(|error| ControlError::new("invalid_request", error.to_string()))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(ControlError::new(
            "request_too_large",
            "The encoded request exceeds 1 MiB.",
        ));
    }
    let sid = user_sid().map_err(unavailable)?;
    let name = pipe_name(directory, &sid).map_err(unavailable)?;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(unavailable)?;
    runtime.block_on(async {
        let open = async {
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(pipe) => return Ok(pipe),
                    Err(error) if error.raw_os_error() == Some(231) => sleep(Duration::from_millis(25)).await,
                    Err(error) => return Err(error),
                }
            }
        };
        let mut pipe = timeout(Duration::from_secs(3), open).await
            .map_err(|_| ControlError::new("app_unavailable", "Rhythm is busy. No request was sent."))?
            .map_err(unavailable)?;
        // Once a write starts, a timeout/disconnect may mean the application already
        // committed. Never retry automatically; a stable request ID enables a safe
        // retry only within this running application's replay ledger.
        let exchange = async {
            pipe.write_u32_le(bytes.len() as u32).await?;
            pipe.write_all(&bytes).await?;
            pipe.flush().await?;
            let length = pipe.read_u32_le().await? as usize;
            if length > MAX_RESPONSE_BYTES { return Err(io::Error::new(io::ErrorKind::InvalidData, "Response exceeds 32 MiB")); }
            let mut bytes = vec![0; length];
            pipe.read_exact(&mut bytes).await?;
            let response: ControlResponse = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if response.protocol_version != PROTOCOL_VERSION || response.request_id != request.request_id { return Err(io::Error::new(io::ErrorKind::InvalidData, "Mismatched response identity")); }
            // The full response is already known even if the final acknowledgment
            // cannot be delivered; do not turn a confirmed result into uncertainty.
            let _ = pipe.write_u8(1).await;
            Ok(response)
        };
        timeout(Duration::from_secs(15), exchange).await
            .map_err(|_| ControlError::new("outcome_unknown", "Rhythm did not reply in time. The action may have completed. Inspect status or retry the same request ID while this app instance remains open."))?
            .map_err(|error| ControlError::new("outcome_unknown", format!("The reply was interrupted; the action may have completed. Inspect status before retrying. {error}")))
    })
}

fn unavailable(error: io::Error) -> ControlError {
    ControlError::new(
        "app_unavailable",
        format!(
            "Open the updated Rhythm desktop app for this data directory. No command was sent. {error}"
        ),
    )
}

async fn read_request(pipe: &mut NamedPipeServer) -> io::Result<ControlRequest> {
    let length = pipe.read_u32_le().await? as usize;
    if length == 0 || length > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Request exceeds 1 MiB",
        ));
    }
    let mut bytes = vec![0; length];
    pipe.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
async fn write_response(pipe: &mut NamedPipeServer, response: &ControlResponse) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(response).map_err(io::Error::other)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        bytes = serde_json::to_vec(&ControlResponse::failure(
            &response.request_id,
            "response_too_large",
            "The action completed, but its response exceeds 32 MiB. Use narrower read commands.",
        ))
        .map_err(io::Error::other)?;
    }
    pipe.write_u32_le(bytes.len() as u32).await?;
    pipe.write_all(&bytes).await?;
    pipe.flush().await
}

fn pipe_name(directory: &Path, sid: &str) -> io::Result<String> {
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()?.join(directory)
    };
    let path = absolute
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let hash = Sha256::digest(format!("{sid}\n{path}").as_bytes());
    Ok(format!(r"\\.\pipe\Rhythm.Control.v1.{hash:x}"))
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: Both conversion APIs below allocate with LocalAlloc; ownership is
        // unique and the pointer is never accessed after this guard is dropped.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: Process pseudo-handle is valid, and token points to a writable HANDLE.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let _token = Token(token);
    let mut needed = 0;
    // SAFETY: A null buffer with zero length queries the required buffer length.
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 provides alignment for TOKEN_USER and the embedded SID storage.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: The buffer has sufficient size/alignment, remains alive through SID
    // conversion, and Windows validates its returned TokenUser layout.
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(text.cast());
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated allocated UTF-16 string.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, length)
    }))
}

fn create_pipe(name: &str, sid: &str, first: bool) -> io::Result<NamedPipeServer> {
    // A protected DACL grants access only to the current user. No Everyone,
    // anonymous, network, or inherited allow entries are present.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated; descriptor points to writable storage and its
    // resulting LocalAlloc allocation is kept alive through pipe creation.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(descriptor);
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: attrs and its descriptor remain valid for the synchronous Windows
    // CreateNamedPipeW call; the kernel copies the descriptor into the pipe object.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            // Windows counts instances retained by disconnected client handles.
            // The server itself owns only two at a time.
            .max_instances(254)
            .in_buffer_size(65536)
            .out_buffer_size(65536)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_pipe_round_trip_replays_and_rejects_a_second_server() {
        let directory = tempfile::tempdir().unwrap();
        let guard = start_server(directory.path(), |r| {
            ControlResponse::success(r.request_id, serde_json::json!({"words":"简体\n繁體"}))
        })
        .unwrap();
        assert!(
            start_server(directory.path(), |r| ControlResponse::success(
                r.request_id,
                Value::Null
            ))
            .is_err()
        );
        let request = ControlRequest {
            protocol_version: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation: "snapshot".into(),
            request: None,
        };
        assert_eq!(
            send(directory.path(), &request).unwrap().result.unwrap()["words"],
            "简体\n繁體"
        );
        drop(guard);
    }

    #[test]
    fn malformed_and_idle_clients_do_not_wedge_the_listener() {
        let directory = tempfile::tempdir().unwrap();
        let _guard = start_server(directory.path(), |r| {
            ControlResponse::success(r.request_id, Value::Null)
        })
        .unwrap();
        let name = pipe_name(directory.path(), &user_sid().unwrap()).unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut malformed = ClientOptions::new().open(&name).unwrap();
            malformed.write_u32_le(1).await.unwrap();
            malformed.write_all(b"{").await.unwrap();
        });
        let request = ControlRequest {
            protocol_version: 1,
            request_id: "after-malformed".into(),
            operation: "status".into(),
            request: None,
        };
        assert!(send(directory.path(), &request).unwrap().ok);
        let idle = {
            let _enter = runtime.enter();
            ClientOptions::new().open(&name).unwrap()
        };
        // A connected client that sends nothing consumes only one bounded slot.
        // The server releases it after five seconds, then answers the waiting request.
        let started = std::time::Instant::now();
        assert!(send(directory.path(), &request).unwrap().ok);
        assert!(started.elapsed() < Duration::from_secs(12));
        drop(idle);
    }

    #[test]
    fn completion_waits_until_the_reply_is_consumed() {
        let directory = tempfile::tempdir().unwrap();
        let (completed, completion) = std::sync::mpsc::channel();
        let _guard = start_server_with_completion(
            directory.path(),
            |request| {
                ControlResponse::success(request.request_id, serde_json::json!({"closing":true}))
            },
            move |response| {
                completed.send(response.clone()).unwrap();
            },
        )
        .unwrap();
        let name = pipe_name(directory.path(), &user_sid().unwrap()).unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut pipe = ClientOptions::new().open(&name).unwrap();
            let request = ControlRequest {
                protocol_version: 1,
                request_id: "acknowledged-quit".into(),
                operation: "quit".into(),
                request: None,
            };
            let bytes = serde_json::to_vec(&request).unwrap();
            pipe.write_u32_le(bytes.len() as u32).await.unwrap();
            pipe.write_all(&bytes).await.unwrap();
            let length = pipe.read_u32_le().await.unwrap() as usize;
            let mut bytes = vec![0; length];
            pipe.read_exact(&mut bytes).await.unwrap();
            let response: ControlResponse = serde_json::from_slice(&bytes).unwrap();
            // Receiving bytes alone does not release application shutdown: an AI
            // client must have a chance to validate and acknowledge the full reply.
            assert!(matches!(
                completion.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            pipe.write_u8(1).await.unwrap();
            assert_eq!(
                completion.recv_timeout(Duration::from_secs(2)).unwrap(),
                response
            );
        });
    }

    #[test]
    fn committed_action_completes_even_if_client_disconnects_before_reply() {
        let directory = tempfile::tempdir().unwrap();
        let (handling, handled) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (completed, completion) = std::sync::mpsc::channel();
        let _guard = start_server_with_completion(
            directory.path(),
            move |request| {
                handling.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(2)).unwrap();
                ControlResponse::success(request.request_id, serde_json::json!({"closing":true}))
            },
            move |response| {
                completed.send(response.clone()).unwrap();
            },
        )
        .unwrap();
        let name = pipe_name(directory.path(), &user_sid().unwrap()).unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut pipe = ClientOptions::new().open(&name).unwrap();
            let request = ControlRequest {
                protocol_version: 1,
                request_id: "abandoned-quit".into(),
                operation: "quit".into(),
                request: None,
            };
            let bytes = serde_json::to_vec(&request).unwrap();
            pipe.write_u32_le(bytes.len() as u32).await.unwrap();
            pipe.write_all(&bytes).await.unwrap();
            handled.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(pipe);
            release.send(()).unwrap();
        });
        let response = completion.recv_timeout(Duration::from_secs(7)).unwrap();
        assert!(response.ok);
        assert_eq!(response.request_id, "abandoned-quit");
    }
}

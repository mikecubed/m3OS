//! Phase 71 — `greeter` binary entry point.
//!
//! GUI login manager: a regular `display_server` client that paints a
//! full-output `Toplevel` surface, reads username + password from the
//! Phase 56 input event stream, authenticates against `/etc/passwd`
//! and `/etc/shadow` (Phase 27 / Phase 48), and emits a one-line
//! session descriptor on stdout for `session_manager` (Phase 71 F.1).
//!
//! The pure-logic modules live in `lib.rs`; this file is the
//! OS-binary wiring.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use alloc::string::{String, ToString};
#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use kernel_core::display::protocol::{
    BufferId, ClientMessage, PROTOCOL_VERSION, Rect, ServerMessage, SurfaceId, SurfaceRole,
};
#[cfg(not(test))]
use kernel_core::input::events::{KeyEvent, KeyEventKind};
#[cfg(not(test))]
use kernel_core::input::keymap::{KEY_BACKSPACE, KEY_ENTER, KEY_ESC, KEY_TAB};
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::{O_RDONLY, STDOUT_FILENO};

#[cfg(not(test))]
use greeter::auth::{AuthBackend, AuthError, AuthLoopState, AuthOutcome, SessionDescriptor};
#[cfg(not(test))]
use greeter::config::{DEFAULT_BACKGROUND_PATHS, GreeterConfig, parse_config};
#[cfg(not(test))]
use greeter::image::{blit_scale_to_fit, decode_bmp, decode_png};
#[cfg(not(test))]
use greeter::render::{ActiveField, LoginUiState, render_login_ui};
#[cfg(not(test))]
use greeter::session_desc::format_session_descriptor;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "greeter: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "greeter: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Default surface dimensions. Matches `term`'s 1280×800 baseline so
/// the greeter inherits the same SHM page count budget.
#[cfg(not(test))]
const SURFACE_WIDTH_PX: u32 = 1280;
#[cfg(not(test))]
const SURFACE_HEIGHT_PX: u32 = 800;

#[cfg(not(test))]
const SURFACE_ID: SurfaceId = SurfaceId(1);
#[cfg(not(test))]
const BUFFER_ID: BufferId = BufferId(1);

#[cfg(not(test))]
const LABEL_VERB: u64 = 1;
#[cfg(not(test))]
const LABEL_CLIENT_EVENT_PULL: u64 = 3;

#[cfg(not(test))]
const VERB_ENCODE_BUF_LEN: usize = 64;

#[cfg(not(test))]
const LOOKUP_BACKOFF_NS: u32 = 5_000_000;
#[cfg(not(test))]
const LOOKUP_MAX_ATTEMPTS: u32 = 2000;

#[cfg(not(test))]
const CONFIG_PATH: &[u8] = b"/etc/greeter.conf\0";

#[cfg(not(test))]
const SERVICE_NAME: &str = "greeter";

#[cfg(not(test))]
const POLL_IDLE_NS: u32 = 5_000_000;

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "greeter: starting (Phase 71)\n");

    // 1. Register an IPC presence beacon so session_manager and
    //    `m3ctl` can observe us.
    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX {
        if let Ok(ep_u32) = u32::try_from(ep) {
            let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
        }
    }

    // 2. Connect to display_server.
    let server_handle = match lookup_display_with_backoff() {
        Some(h) => h,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "greeter: display_server unavailable\n");
            return 2;
        }
    };

    // 3. Allocate the surface backing buffer.
    let byte_len = (SURFACE_WIDTH_PX as usize) * (SURFACE_HEIGHT_PX as usize) * 4;
    let shm_id = syscall_lib::shm_create(byte_len);
    if shm_id == 0 {
        syscall_lib::write_str(STDOUT_FILENO, "greeter: shm_create failed\n");
        return 3;
    }
    let surface_va = syscall_lib::shm_map(shm_id);
    if surface_va == 0 {
        syscall_lib::write_str(STDOUT_FILENO, "greeter: shm_map failed\n");
        let _ = syscall_lib::shm_destroy(shm_id);
        return 3;
    }
    let surface_len = byte_len.div_ceil(4096) * 4096;

    // 4. Phase 56 handshake.
    if !send_hello_and_create_surface(server_handle) {
        let _ = syscall_lib::shm_unmap(surface_va);
        let _ = syscall_lib::shm_destroy(shm_id);
        return 4;
    }

    // 5. Load greeter config + background image (best-effort).
    let mut config_events: Vec<greeter::config::ConfigParseEvent> = Vec::new();
    let config = load_config(&mut config_events);
    for ev in &config_events {
        log_config_event(ev);
    }
    let background = load_background_image(&config);

    // Paint the initial frame (background only) so the surface has a
    // committed buffer before we begin draining input events.
    let pixels_slice = unsafe { surface_pixels(surface_va, surface_len) };
    paint_background(
        pixels_slice,
        SURFACE_WIDTH_PX,
        SURFACE_HEIGHT_PX,
        &background,
        &config,
    );

    let initial_state = LoginUiState {
        config: &config,
        username: "",
        password_len: 0,
        active: ActiveField::Username,
        error: None,
        backoff_seconds_remaining: None,
    };
    render_login_ui(
        &initial_state,
        pixels_slice,
        SURFACE_WIDTH_PX,
        SURFACE_HEIGHT_PX,
    );
    if !attach_damage_commit(server_handle, shm_id) {
        let _ = syscall_lib::shm_unmap(surface_va);
        let _ = syscall_lib::shm_destroy(shm_id);
        return 5;
    }

    // 6. Auth loop. Repeats until a valid credential set is entered.
    let descriptor = match run_auth_loop(server_handle, pixels_slice, shm_id, &background, &config)
    {
        Some(d) => d,
        None => {
            let _ = syscall_lib::shm_unmap(surface_va);
            let _ = syscall_lib::shm_destroy(shm_id);
            return 6;
        }
    };

    // 7. Emit session descriptor on stdout for observability (Phase 71
    //    F.1 — session_manager parses the same line from its captured
    //    stdout when the orchestrator is wired through fork+pipe; in
    //    the simpler in-process design we exec straight into term).
    let line = format_session_descriptor(&descriptor);
    let _ = syscall_lib::write(STDOUT_FILENO, line.as_bytes());

    // Release the surface before exec so the next process has a clean
    // slot to claim. The exec replaces the address space; the kernel
    // would tear the SHM mapping down anyway, but releasing now
    // surfaces any leak in `shm_destroy` as a greeter-side log line.
    let _ = syscall_lib::shm_unmap(surface_va);
    let _ = syscall_lib::shm_destroy(shm_id);

    // Tell `display_server` we're done so it releases the surface +
    // any focus state attached to this client. Best-effort; the
    // server cleans up on disconnect anyway.
    let _ = send_verb(server_handle, &ClientMessage::Goodbye);

    // Phase 71 F.2 — drop privileges before exec'ing the user shell so
    // `term` and every descendant runs under the authenticated UID/GID.
    if syscall_lib::setgid(descriptor.gid) != 0 {
        syscall_lib::write_str(STDOUT_FILENO, "greeter: setgid failed\n");
        return 7;
    }
    if syscall_lib::setuid(descriptor.uid) != 0 {
        syscall_lib::write_str(STDOUT_FILENO, "greeter: setuid failed\n");
        return 7;
    }

    // Build envp for term — same vars the autologin path passes.
    let mut home_env_buf = [0u8; 256];
    let home_env_len = build_env_string(b"HOME=", descriptor.home.as_bytes(), &mut home_env_buf);
    let env_path: &[u8] = b"PATH=/usr/local/bin:/bin:/sbin:/usr/bin\0";
    let env_term: &[u8] = b"TERM=m3os-term\0";
    let env_editor: &[u8] = b"EDITOR=/bin/edit\0";
    let envp: [*const u8; 5] = [
        env_path.as_ptr(),
        home_env_buf[..home_env_len].as_ptr(),
        env_term.as_ptr(),
        env_editor.as_ptr(),
        core::ptr::null(),
    ];

    let term_path: &[u8] = b"/bin/term\0";
    let argv: [*const u8; 2] = [term_path.as_ptr(), core::ptr::null()];
    let ret = syscall_lib::execve(term_path, &argv, &envp);
    syscall_lib::write_str(STDOUT_FILENO, "greeter: execve /bin/term failed: ");
    syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
    8
}

/// Format `KEY=value\0` into `out`. Returns bytes written including
/// the trailing NUL. Truncates silently if `out` is too small.
#[cfg(not(test))]
fn build_env_string(prefix: &[u8], value: &[u8], out: &mut [u8]) -> usize {
    let mut pos = 0usize;
    for &b in prefix {
        if pos >= out.len() {
            break;
        }
        out[pos] = b;
        pos += 1;
    }
    for &b in value {
        if pos >= out.len() {
            break;
        }
        out[pos] = b;
        pos += 1;
    }
    if pos >= out.len() {
        out[out.len() - 1] = 0;
        return out.len();
    }
    out[pos] = 0;
    pos + 1
}

#[cfg(not(test))]
fn lookup_display_with_backoff() -> Option<u32> {
    for attempt in 0..LOOKUP_MAX_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service("display");
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == LOOKUP_MAX_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, LOOKUP_BACKOFF_NS);
    }
    None
}

#[cfg(not(test))]
fn send_verb(handle: u32, msg: &ClientMessage) -> bool {
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let len = match msg.encode(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let reply = syscall_lib::ipc_call_buf(handle, LABEL_VERB, 0, &buf[..len]);
    reply != u64::MAX
}

#[cfg(not(test))]
fn send_hello_and_create_surface(handle: u32) -> bool {
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: 0,
    };
    if !send_verb(handle, &hello) {
        return false;
    }
    let create = ClientMessage::CreateSurface {
        surface_id: SURFACE_ID,
    };
    if !send_verb(handle, &create) {
        return false;
    }
    let role = ClientMessage::SetSurfaceRole {
        surface_id: SURFACE_ID,
        role: SurfaceRole::Toplevel,
    };
    if !send_verb(handle, &role) {
        return false;
    }
    true
}

#[cfg(not(test))]
fn attach_damage_commit(handle: u32, shm_id: u32) -> bool {
    let attach = ClientMessage::AttachSharedBuffer {
        surface_id: SURFACE_ID,
        buffer_id: BUFFER_ID,
        shm_id,
        width: SURFACE_WIDTH_PX,
        height: SURFACE_HEIGHT_PX,
    };
    if !send_verb(handle, &attach) {
        return false;
    }
    let damage = ClientMessage::DamageSurface {
        surface_id: SURFACE_ID,
        rect: Rect {
            x: 0,
            y: 0,
            w: SURFACE_WIDTH_PX,
            h: SURFACE_HEIGHT_PX,
        },
    };
    if !send_verb(handle, &damage) {
        return false;
    }
    let commit = ClientMessage::CommitSurface {
        surface_id: SURFACE_ID,
    };
    if !send_verb(handle, &commit) {
        return false;
    }
    true
}

#[cfg(not(test))]
unsafe fn surface_pixels(surface_va: u64, surface_len: usize) -> &'static mut [u32] {
    // SAFETY: caller holds the SHM mapping for `surface_len` bytes.
    // Cast u8 to u32: SurfaceCreate allocates 4 KiB-aligned pages, so
    // alignment is satisfied.
    let pixel_count = surface_len / 4;
    unsafe { core::slice::from_raw_parts_mut(surface_va as *mut u32, pixel_count) }
}

/// Best-effort config loader. Missing file → built-in defaults silently.
#[cfg(not(test))]
fn load_config(events: &mut Vec<greeter::config::ConfigParseEvent>) -> GreeterConfig {
    let mut buf = [0u8; 4096];
    let n = read_file(CONFIG_PATH, &mut buf);
    if n == 0 {
        return GreeterConfig::default();
    }
    let text = match core::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "greeter: config not utf-8; using defaults\n");
            return GreeterConfig::default();
        }
    };
    parse_config(text, events)
}

#[cfg(not(test))]
fn log_config_event(ev: &greeter::config::ConfigParseEvent) {
    use greeter::config::ConfigParseEvent::*;
    match ev {
        UnknownKey(k) => {
            syscall_lib::write_str(STDOUT_FILENO, "greeter: warn: unknown config key '");
            let _ = syscall_lib::write(STDOUT_FILENO, k.as_bytes());
            syscall_lib::write_str(STDOUT_FILENO, "'\n");
        }
        InvalidColor { key, value } => {
            syscall_lib::write_str(STDOUT_FILENO, "greeter: warn: invalid color for '");
            let _ = syscall_lib::write(STDOUT_FILENO, key.as_bytes());
            syscall_lib::write_str(STDOUT_FILENO, "': '");
            let _ = syscall_lib::write(STDOUT_FILENO, value.as_bytes());
            syscall_lib::write_str(STDOUT_FILENO, "'\n");
        }
    }
}

/// Decoded background image, or `None` if no file was loadable.
#[cfg(not(test))]
struct Background {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

#[cfg(not(test))]
fn load_background_image(config: &GreeterConfig) -> Option<Background> {
    let candidates: Vec<&str> = match &config.background {
        Some(p) => alloc::vec![p.as_str()],
        None => DEFAULT_BACKGROUND_PATHS.iter().copied().collect(),
    };
    for path in candidates {
        let mut path_buf = [0u8; 256];
        if path.len() >= path_buf.len() {
            continue;
        }
        path_buf[..path.len()].copy_from_slice(path.as_bytes());
        path_buf[path.len()] = 0;
        let mut bytes = Vec::new();
        if !read_file_into_vec(&path_buf[..path.len() + 1], &mut bytes) {
            continue;
        }
        let decoded = if path.ends_with(".bmp") {
            decode_bmp(&bytes)
        } else {
            decode_png(&bytes)
        };
        match decoded {
            Ok((w, h, pixels)) => {
                return Some(Background {
                    pixels,
                    width: w,
                    height: h,
                });
            }
            Err(_) => {
                syscall_lib::write_str(STDOUT_FILENO, "greeter: warn: failed to decode '");
                let _ = syscall_lib::write(STDOUT_FILENO, path.as_bytes());
                syscall_lib::write_str(STDOUT_FILENO, "'\n");
            }
        }
    }
    None
}

#[cfg(not(test))]
fn paint_background(
    pixels: &mut [u32],
    width: u32,
    height: u32,
    background: &Option<Background>,
    config: &GreeterConfig,
) {
    match background {
        Some(bg) => {
            blit_scale_to_fit(&bg.pixels, bg.width, bg.height, pixels, width, height);
        }
        None => {
            for px in pixels.iter_mut() {
                *px = greeter::config::DEFAULT_BACKGROUND_COLOR;
            }
        }
    }
    let _ = config;
}

/// Read at most `buf.len()` bytes of `path` into `buf`. Returns count.
#[cfg(not(test))]
fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = syscall_lib::open(path, O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0usize;
    loop {
        if total >= buf.len() {
            break;
        }
        let n = syscall_lib::read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    let _ = syscall_lib::close(fd as i32);
    total
}

/// Read the entire contents of `path` into `out`. Returns `true` on
/// success.
#[cfg(not(test))]
fn read_file_into_vec(path: &[u8], out: &mut Vec<u8>) -> bool {
    let fd = syscall_lib::open(path, O_RDONLY, 0);
    if fd < 0 {
        return false;
    }
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd as i32, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd as i32);
            return false;
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd as i32);
    true
}

// =========================================================================
// Auth loop
// =========================================================================

/// Production [`AuthBackend`] backed by `/etc/passwd` + `/etc/shadow`.
#[cfg(not(test))]
struct FsAuthBackend;

#[cfg(not(test))]
impl AuthBackend for FsAuthBackend {
    fn verify(&self, username: &str, password: &str) -> Result<SessionDescriptor, AuthError> {
        let mut passwd_buf = [0u8; 4096];
        let passwd_len = read_file(b"/etc/passwd\0", &mut passwd_buf);
        if passwd_len == 0 {
            return Err(AuthError::StoreUnavailable);
        }
        let (uid, gid, home, shell) =
            match find_user(&passwd_buf[..passwd_len], username.as_bytes()) {
                Some(v) => v,
                None => return Err(AuthError::UnknownUser),
            };
        let mut shadow_buf = [0u8; 4096];
        let shadow_len = read_file(b"/etc/shadow\0", &mut shadow_buf);
        if shadow_len == 0 {
            return Err(AuthError::StoreUnavailable);
        }
        if account_is_locked(&shadow_buf[..shadow_len], username.as_bytes()) {
            return Err(AuthError::AccountLocked);
        }
        if !verify_shadow_password(
            &shadow_buf[..shadow_len],
            username.as_bytes(),
            password.as_bytes(),
        ) {
            return Err(AuthError::BadPassword);
        }
        Ok(SessionDescriptor {
            uid,
            gid,
            home: bytes_to_string(home),
            shell: bytes_to_string(shell),
        })
    }
}

#[cfg(not(test))]
fn bytes_to_string(b: &[u8]) -> String {
    match core::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(not(test))]
fn find_user<'a>(passwd: &'a [u8], username: &[u8]) -> Option<(u32, u32, &'a [u8], &'a [u8])> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = match split_colon(line) {
            Some(f) => f,
            None => continue,
        };
        if fields[0] == username {
            let uid = parse_u32(fields[2])?;
            let gid = parse_u32(fields[3])?;
            return Some((uid, gid, fields[5], fields[6]));
        }
    }
    None
}

#[cfg(not(test))]
fn split_colon(line: &[u8]) -> Option<[&[u8]; 7]> {
    let mut fields = [&[] as &[u8]; 7];
    let mut start = 0;
    let mut field = 0;
    for (i, &b) in line.iter().enumerate() {
        if b == b':' {
            if field >= 7 {
                return None;
            }
            fields[field] = &line[start..i];
            field += 1;
            start = i + 1;
        }
    }
    if field == 6 {
        fields[6] = &line[start..];
        Some(fields)
    } else {
        None
    }
}

#[cfg(not(test))]
fn parse_u32(s: &[u8]) -> Option<u32> {
    let mut n: u32 = 0;
    let mut saw = false;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        saw = true;
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    if saw { Some(n) } else { None }
}

#[cfg(not(test))]
fn account_is_locked(shadow: &[u8], username: &[u8]) -> bool {
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            let name = &line[..colon];
            if name == username {
                let rest = &line[colon + 1..];
                let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
                let hash_field = &rest[..hash_end];
                return hash_field == b"!" || hash_field == b"*";
            }
        }
    }
    false
}

#[cfg(not(test))]
fn verify_shadow_password(shadow: &[u8], username: &[u8], password: &[u8]) -> bool {
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            let name = &line[..colon];
            if name == username {
                let rest = &line[colon + 1..];
                let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
                let hash_field = &rest[..hash_end];
                return syscall_lib::sha256::verify_password(password, hash_field);
            }
        }
    }
    false
}

#[cfg(not(test))]
fn run_auth_loop(
    handle: u32,
    pixels: &mut [u32],
    shm_id: u32,
    background: &Option<Background>,
    config: &GreeterConfig,
) -> Option<SessionDescriptor> {
    let mut state = AuthLoopState::new();
    let backend = FsAuthBackend;
    loop {
        let username = match read_field(
            handle,
            shm_id,
            ActiveField::Username,
            pixels,
            background,
            config,
        ) {
            Some(s) => s,
            None => {
                // Disconnect or unrecoverable error.
                return None;
            }
        };
        let password = match read_field(
            handle,
            shm_id,
            ActiveField::Password,
            pixels,
            background,
            config,
        ) {
            Some(s) => s,
            None => return None,
        };
        // Commit a "checking..." frame so the user gets feedback.
        let checking = LoginUiState {
            config,
            username: username.as_str(),
            password_len: password.len(),
            active: ActiveField::Password,
            error: Some("Authenticating..."),
            backoff_seconds_remaining: None,
        };
        paint_background(
            pixels,
            SURFACE_WIDTH_PX,
            SURFACE_HEIGHT_PX,
            background,
            config,
        );
        render_login_ui(&checking, pixels, SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX);
        let _ = attach_damage_commit(handle, shm_id);

        let result = backend.verify(&username, &password);
        let outcome = state.record_attempt(result);
        match outcome {
            AuthOutcome::Success(desc) => return Some(desc),
            AuthOutcome::Failed(err) => {
                let msg = error_to_message(&err);
                let failed = LoginUiState {
                    config,
                    username: "",
                    password_len: 0,
                    active: ActiveField::Username,
                    error: Some(msg),
                    backoff_seconds_remaining: None,
                };
                paint_background(
                    pixels,
                    SURFACE_WIDTH_PX,
                    SURFACE_HEIGHT_PX,
                    background,
                    config,
                );
                render_login_ui(&failed, pixels, SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX);
                let _ = attach_damage_commit(handle, shm_id);
            }
            AuthOutcome::Backoff { wait_secs, reason } => {
                let msg = error_to_message(&reason);
                // Show countdown ticking down once per second so the
                // user sees the wait progressing.
                let mut remaining = wait_secs;
                while remaining > 0 {
                    let backoff_state = LoginUiState {
                        config,
                        username: "",
                        password_len: 0,
                        active: ActiveField::Username,
                        error: Some(msg),
                        backoff_seconds_remaining: Some(remaining),
                    };
                    paint_background(
                        pixels,
                        SURFACE_WIDTH_PX,
                        SURFACE_HEIGHT_PX,
                        background,
                        config,
                    );
                    render_login_ui(&backoff_state, pixels, SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX);
                    let _ = attach_damage_commit(handle, shm_id);
                    let _ = syscall_lib::nanosleep_for(1, 0);
                    remaining = remaining.saturating_sub(1);
                }
            }
        }
    }
}

#[cfg(not(test))]
fn error_to_message(err: &AuthError) -> &'static str {
    match err {
        AuthError::UnknownUser => "Login incorrect",
        AuthError::BadPassword => "Login incorrect",
        AuthError::AccountLocked => "Account locked; set password from serial console first.",
        AuthError::StoreUnavailable => "Cannot read user database",
    }
}

/// Read one form field from the keyboard event stream. Returns the
/// typed string when the user submits with Enter; `None` if the
/// display server disconnects.
#[cfg(not(test))]
fn read_field(
    handle: u32,
    shm_id: u32,
    active: ActiveField,
    pixels: &mut [u32],
    background: &Option<Background>,
    config: &GreeterConfig,
) -> Option<String> {
    let mut buf = String::new();
    let mut event_buf = [0u8; 64];
    let mut dirty = true;
    loop {
        if dirty {
            // Repaint to show the current buffer contents (username
            // only; password always shows empty per Phase 71 D.1).
            paint_background(
                pixels,
                SURFACE_WIDTH_PX,
                SURFACE_HEIGHT_PX,
                background,
                config,
            );
            let ui_state = LoginUiState {
                config,
                username: if matches!(active, ActiveField::Username) {
                    buf.as_str()
                } else {
                    ""
                },
                password_len: if matches!(active, ActiveField::Password) {
                    buf.len()
                } else {
                    0
                },
                active,
                error: None,
                backoff_seconds_remaining: None,
            };
            render_login_ui(&ui_state, pixels, SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX);
            let _ = attach_damage_commit(handle, shm_id);
            dirty = false;
        }

        match pull_one_event(handle, &mut event_buf) {
            PulledEvent::Key(ev) => {
                if ev.kind == KeyEventKind::Up {
                    continue;
                }
                match handle_key(&ev, &mut buf) {
                    KeyAction::Submit => return Some(buf),
                    KeyAction::Continue => {
                        dirty = true;
                    }
                    KeyAction::Cancel => {
                        buf.clear();
                        dirty = true;
                    }
                }
            }
            PulledEvent::Disconnect => return None,
            PulledEvent::None | PulledEvent::Other => {
                let _ = syscall_lib::nanosleep_for(0, POLL_IDLE_NS);
            }
        }
    }
}

#[cfg(not(test))]
enum PulledEvent {
    Key(KeyEvent),
    Disconnect,
    Other,
    None,
}

#[cfg(not(test))]
fn pull_one_event(handle: u32, buf: &mut [u8]) -> PulledEvent {
    let label = syscall_lib::ipc_call(handle, LABEL_CLIENT_EVENT_PULL, SURFACE_ID.0 as u64);
    if label != LABEL_CLIENT_EVENT_PULL {
        let _ = syscall_lib::ipc_take_pending_bulk(buf);
        return PulledEvent::None;
    }
    let n = syscall_lib::ipc_take_pending_bulk(buf);
    if n == 0 || n == u64::MAX {
        return PulledEvent::None;
    }
    let len = n as usize;
    if len > buf.len() {
        return PulledEvent::None;
    }
    match ServerMessage::decode(&buf[..len]) {
        Ok((ServerMessage::Key(ev), _)) => PulledEvent::Key(ev),
        Ok((ServerMessage::Disconnect { .. }, _)) => PulledEvent::Disconnect,
        Ok(_) => PulledEvent::Other,
        Err(_) => PulledEvent::None,
    }
}

#[cfg(not(test))]
enum KeyAction {
    Submit,
    Continue,
    Cancel,
}

#[cfg(not(test))]
fn handle_key(ev: &KeyEvent, buf: &mut String) -> KeyAction {
    let keycode = ev.keycode;
    if keycode == KEY_ENTER.0 {
        return KeyAction::Submit;
    }
    if keycode == KEY_BACKSPACE.0 {
        let _ = buf.pop();
        return KeyAction::Continue;
    }
    if keycode == KEY_TAB.0 {
        // Treat Tab as Enter to advance: the sequential read_field
        // contract walks the two fields anyway, so confirming with
        // Tab gets the user to the next field as they'd expect.
        return KeyAction::Submit;
    }
    if keycode == KEY_ESC.0 {
        return KeyAction::Cancel;
    }
    // For Ctrl+C / Ctrl+D — also cancel.
    if ev.modifiers.bits() & kernel_core::input::events::MOD_CTRL != 0 {
        if let Some(ch) = char::from_u32(ev.symbol) {
            if ch == 'c' || ch == 'C' || ch == 'd' || ch == 'D' {
                return KeyAction::Cancel;
            }
        }
    }
    // Printable characters from the keymap.
    if let Some(ch) = char::from_u32(ev.symbol) {
        if (ev.symbol >= 0x20 && ev.symbol < 0x7F) && buf.len() < 128 {
            buf.push(ch);
        }
    }
    KeyAction::Continue
}

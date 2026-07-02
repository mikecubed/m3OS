//! Minimal `audio_server` client over the raw IPC syscalls.
//!
//! A re-expression of the Phase 57 audio wire protocol
//! (`kernel-core/src/audio/protocol.rs`) and the transport shape of
//! `userspace/lib/audio_client` (`SyscallSocket::call`: the request
//! frame and the PCM bulk ride ONE `ipc_call_buf` as `frame ++ bulk`;
//! the reply is drained with `ipc_take_pending_bulk`). Those crates are
//! `x86_64-unknown-none` workspace members a musl crate cannot link, so
//! the wire constants are re-declared with provenance comments.
//!
//! The submit loop mirrors `userspace/audio-demo`'s `submit_tone`:
//! ≤8 KiB frame-aligned chunks (half the AC'97 BDL ring), bounded retry
//! on `WouldBlock` (ring full) and on the documented transient
//! `ipc_call_buf → u64::MAX` race.

use crate::m3ipc;

/// `audio_server::SERVICE_NAME` (userspace/audio_server/src/lib.rs:58).
const SERVICE_NAME: &str = "audio.cmd";
/// `audio_client::LABEL_AUDIO_CMD` (userspace/lib/audio_client/src/lib.rs:42).
const LABEL_AUDIO_CMD: u64 = 0x000A_0D10_C0DE;

// Frame header: [body_len: u16 LE][opcode: u16 LE][body]
// (kernel-core/src/audio/protocol.rs:47, FRAME_HEADER_SIZE = 4).
const OP_CLIENT_OPEN: u16 = 0x0001; // protocol.rs:64
const OP_CLIENT_SUBMIT_FRAMES: u16 = 0x0002; // protocol.rs:65
const OP_CLIENT_DRAIN: u16 = 0x0003; // protocol.rs:66
const OP_CLIENT_CLOSE: u16 = 0x0004; // protocol.rs:67

const OP_SERVER_OPENED: u16 = 0x0101; // protocol.rs:78
const OP_SERVER_OPEN_ERROR: u16 = 0x0102; // protocol.rs:79
const OP_SERVER_SUBMIT_ACK: u16 = 0x0103; // protocol.rs:80
const OP_SERVER_SUBMIT_ERROR: u16 = 0x0104; // protocol.rs:81
const OP_SERVER_DRAIN_ACK: u16 = 0x0105; // protocol.rs:82
const OP_SERVER_CLOSED: u16 = 0x0106; // protocol.rs:83

// Open body byte tags (protocol.rs:161-164).
const TAG_FMT_S16LE: u8 = 0;
const TAG_LAYOUT_STEREO: u8 = 1;
const TAG_RATE_48000: u8 = 0;

/// `AudioError` wire byte 1 = WouldBlock (protocol.rs — transient,
/// DMA ring full; retry).
const ERR_WOULD_BLOCK: u8 = 1;

/// `MAX_SUBMIT_BYTES` is 64 KiB (protocol.rs:57); audio-demo caps each
/// submit at 8 KiB (half the AC'97 BDL ring) so a fresh chunk always
/// fits mid-playback — mirror that.
const SUBMIT_CHUNK_BYTES: usize = 8 * 1024;
const STEREO_FRAME_BYTES: usize = 4;

const MAX_RETRIES: usize = 200;
const RETRY_SLEEP_MS: u64 = 5;

fn frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&opcode.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// One IPC round-trip: send `frame ++ bulk`, return the reply bytes.
/// `Err` carries a stable reason label.
fn call(ep: u32, frame_bytes: &[u8], bulk: &[u8]) -> Result<Vec<u8>, String> {
    let mut combined = Vec::with_capacity(frame_bytes.len() + bulk.len());
    combined.extend_from_slice(frame_bytes);
    combined.extend_from_slice(bulk);
    let reply_label = m3ipc::ipc_call_buf(ep, LABEL_AUDIO_CMD, LABEL_AUDIO_CMD, &combined);
    if reply_label == u64::MAX {
        return Err("ipc-call".to_string()); // transient EPIPE-shaped race
    }
    let mut reply = [0u8; 64];
    let n = m3ipc::ipc_take_pending_bulk(&mut reply);
    if n == u64::MAX {
        return Err("ipc-reply".to_string());
    }
    Ok(reply[..(n as usize).min(reply.len())].to_vec())
}

/// Parse `[body_len][opcode][body]`; returns (opcode, body).
fn parse_reply(reply: &[u8]) -> Result<(u16, &[u8]), String> {
    if reply.len() < 4 {
        return Err("short-reply".to_string());
    }
    let body_len = u16::from_le_bytes([reply[0], reply[1]]) as usize;
    let opcode = u16::from_le_bytes([reply[2], reply[3]]);
    if reply.len() < 4 + body_len {
        return Err("truncated-reply".to_string());
    }
    Ok((opcode, &reply[4..4 + body_len]))
}

/// Play interleaved S16LE stereo 48 kHz samples through `audio_server`:
/// Open → chunked SubmitFrames → Drain → Close.
pub fn play_s16le_stereo_48k(samples: &[i16]) -> Result<(), String> {
    let ep_handle = m3ipc::ipc_lookup_service(SERVICE_NAME);
    if ep_handle == u64::MAX {
        return Err("no-audio-service".to_string());
    }
    let ep = u32::try_from(ep_handle).map_err(|_| "bad-endpoint".to_string())?;

    // Open(S16LE, Stereo, 48 kHz) — body [fmt, layout, rate] (protocol.rs:420).
    let open = frame(
        OP_CLIENT_OPEN,
        &[TAG_FMT_S16LE, TAG_LAYOUT_STEREO, TAG_RATE_48000],
    );
    let reply = call(ep, &open, &[])?;
    match parse_reply(&reply)? {
        (OP_SERVER_OPENED, _) => {}
        (OP_SERVER_OPEN_ERROR, body) => {
            return Err(format!("open-error-{}", body.first().copied().unwrap_or(255)));
        }
        (op, _) => return Err(format!("open-unexpected-{op:#06x}")),
    }

    // Byte view of the PCM.
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        pcm.extend_from_slice(&s.to_le_bytes());
    }

    let mut offset = 0usize;
    while offset < pcm.len() {
        let chunk_len = (pcm.len() - offset)
            .min(SUBMIT_CHUNK_BYTES / STEREO_FRAME_BYTES * STEREO_FRAME_BYTES);
        let chunk = &pcm[offset..offset + chunk_len];
        // SubmitFrames { len } — body u32 LE (protocol.rs:429); PCM rides
        // the same call as trailing bulk.
        let submit = frame(OP_CLIENT_SUBMIT_FRAMES, &(chunk_len as u32).to_le_bytes());

        let mut retries = 0usize;
        loop {
            match call(ep, &submit, chunk) {
                Ok(reply) => match parse_reply(&reply)? {
                    (OP_SERVER_SUBMIT_ACK, _) => break,
                    (OP_SERVER_SUBMIT_ERROR, body)
                        if body.first() == Some(&ERR_WOULD_BLOCK) =>
                    {
                        // AC'97 BDL ring full — drains in ~ms; bounded retry
                        // (mirrors audio-demo's submit_tone loop).
                        if retries >= MAX_RETRIES {
                            let _ = close(ep);
                            return Err("would-block-stuck".to_string());
                        }
                        retries += 1;
                        crate::m3ipc::nanosleep_ms(RETRY_SLEEP_MS);
                    }
                    (OP_SERVER_SUBMIT_ERROR, body) => {
                        let _ = close(ep);
                        return Err(format!(
                            "submit-error-{}",
                            body.first().copied().unwrap_or(255)
                        ));
                    }
                    (op, _) => {
                        let _ = close(ep);
                        return Err(format!("submit-unexpected-{op:#06x}"));
                    }
                },
                // Documented transient ipc_call_buf race (audio-demo's
                // Io(-32) arm) — same bounded backoff.
                Err(ref e) if e == "ipc-call" => {
                    if retries >= MAX_RETRIES {
                        let _ = close(ep);
                        return Err("ipc-call-stuck".to_string());
                    }
                    retries += 1;
                    crate::m3ipc::nanosleep_ms(RETRY_SLEEP_MS);
                }
                Err(e) => {
                    let _ = close(ep);
                    return Err(e);
                }
            }
        }
        offset += chunk_len;
    }

    // Drain — block until the device consumed everything.
    let reply = call(ep, &frame(OP_CLIENT_DRAIN, &[]), &[])?;
    match parse_reply(&reply)? {
        (OP_SERVER_DRAIN_ACK, _) => {}
        (op, _) => {
            let _ = close(ep);
            return Err(format!("drain-unexpected-{op:#06x}"));
        }
    }

    close(ep)
}

fn close(ep: u32) -> Result<(), String> {
    let reply = call(ep, &frame(OP_CLIENT_CLOSE, &[]), &[])?;
    match parse_reply(&reply)? {
        (OP_SERVER_CLOSED, _) => Ok(()),
        (op, _) => Err(format!("close-unexpected-{op:#06x}")),
    }
}

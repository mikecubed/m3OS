/*
 * display_client.h — Phase 70 Track A0 C-ABI header.
 *
 * Hand-written companion to userspace/lib/display_client_ffi/src/lib.rs.
 * The crate's build.rs verifies every DC_* and DC_EVENT_* #define against
 * the corresponding `pub const` in src/lib.rs at compile time; a mismatch
 * fails the build with a `panic!()`.
 *
 * Wraps the Phase 56 ClientMessage codec + the shared-memory pixel-buffer
 * lifecycle behind a minimal verb set so DOOM (which is C) can speak the
 * surface-buffer protocol without hand-encoding bytes.
 */
#ifndef DISPLAY_CLIENT_H
#define DISPLAY_CLIENT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---------- Error codes (negative `int`s, mirrored by `pub const` in lib.rs) */

#define DC_OK 0
/* Service lookup, socket open, or Hello round-trip failed. */
#define DC_ERR_CONNECT -1
/* Encode of an outbound `ClientMessage` body failed. */
#define DC_ERR_ENCODE -2
/* `ipc_call_buf` returned u64::MAX (transport error). */
#define DC_ERR_IPC -3
/* Argument validation failed (null pointer, zero dimension, ...). */
#define DC_ERR_INVALID_ARG -4
/* Null handle passed to a verb that requires it. */
#define DC_ERR_NULL_HANDLE -5
/* Protocol decode of a server-pushed event failed. */
#define DC_ERR_PROTOCOL -6

/* ---------- Event tag values (mirrored by `pub const` in lib.rs) */

#define DC_EVENT_NONE 0
#define DC_EVENT_KEY 1
#define DC_EVENT_FOCUS_IN 2
#define DC_EVENT_FOCUS_OUT 3
#define DC_EVENT_SURFACE_RESIZED 4
#define DC_EVENT_BUFFER_RELEASED 5
#define DC_EVENT_DISCONNECT 6
/* Phase 72b Track K.6 — SUPER+Q graceful close request. */
#define DC_EVENT_CLOSE_REQUEST 7

/* ---------- Key event kind (mirrors `KeyEventKind` discriminants) */

#define DC_KEY_KIND_DOWN 0
#define DC_KEY_KIND_UP 1
#define DC_KEY_KIND_REPEAT 2

/* Opaque handle returned by `dc_connect`. */
typedef struct DcHandle DcHandle;

/* Tagged union of subscribable server events. Memory layout is
 * `#[repr(C)]` on the Rust side; C callers branch on `tag` and then
 * read the corresponding member of `payload`. */
typedef struct {
    /* `DC_EVENT_*` discriminant. */
    uint32_t tag;
    union {
        struct {
            uint64_t timestamp_ms;
            uint32_t keycode;
            uint32_t symbol;
            uint16_t modifiers;
            uint8_t kind;          /* DC_KEY_KIND_* */
            uint8_t modifier_side; /* Phase 68: 0=Left, 1=Right, 2=Either */
        } key;
        struct {
            uint32_t surface_id;
        } focus_in;
        struct {
            uint32_t surface_id;
        } focus_out;
        struct {
            uint32_t surface_id;
            uint32_t width;
            uint32_t height;
        } surface_resized;
        struct {
            uint32_t surface_id;
            uint32_t buffer_id;
        } buffer_released;
        struct {
            /* DisconnectReason discriminant — informational only. */
            uint32_t reason;
        } disconnect;
    } payload;
} DcEvent;

/* ---------- C-ABI verbs */

/* Resolve the `"display"` registered service with bounded retry, open
 * the IPC channel, and send `ClientMessage::Hello { protocol_version =
 * PROTOCOL_VERSION }`. Returns DC_OK on success or a negative DC_ERR_*
 * code; on success `*out` is populated with an opaque handle that must
 * be freed via `dc_disconnect`. */
int dc_connect(DcHandle **out);

/* Create a Toplevel surface bound to this handle. Sends
 * `CreateSurface { surface_id }` followed by
 * `SetSurfaceRole { surface_id, role: Toplevel }`. The implementation
 * picks the surface id; on success `*out_surface_id` receives it. */
int dc_create_toplevel(DcHandle *h, uint32_t *out_surface_id);

/* Attach a client-allocated SHM region as the pixel buffer for the
 * given surface. The caller previously obtained `shm_id` via
 * `sys_shm_create(width * height * 4)` and mapped the region via
 * `sys_shm_map`. Sends `AttachSharedBuffer { surface_id, buffer_id,
 * shm_id, width, height }`. */
int dc_attach_shm_buffer(DcHandle *h,
                         uint32_t surface_id,
                         uint32_t buffer_id,
                         uint32_t shm_id,
                         uint32_t width,
                         uint32_t height);

/* Send `DamageSurface { surface_id, rect: { x, y, w, h } }` followed
 * by `CommitSurface { surface_id }`. Returns DC_OK on success.
 * The rect's height is named `height_px` so the parameter list does
 * not collide with the `DcHandle *h` first parameter. */
int dc_damage_and_commit(DcHandle *h,
                         uint32_t surface_id,
                         int32_t x,
                         int32_t y,
                         uint32_t w,
                         uint32_t height_px);

/* Non-blocking server-event drain. Returns 1 if `*out` was populated
 * with a decoded event, 0 if no event was ready, or a negative
 * DC_ERR_* code on transport / decode failure. */
int dc_poll_event(DcHandle *h, DcEvent *out);

/* Send `Goodbye`, close the IPC handle, and free the wrapper. After
 * return the pointer must not be reused. Passing NULL is a safe no-op. */
void dc_disconnect(DcHandle *h);

#ifdef __cplusplus
}
#endif

#endif /* DISPLAY_CLIENT_H */

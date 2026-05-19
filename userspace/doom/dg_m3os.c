/*
 * dg_m3os.c — Phase 70 doomgeneric platform layer for m3OS.
 *
 * Phase 70 turned DOOM into a regular `display_server` client. The
 * legacy fb-takeover path (`sys_framebuffer_acquire` + mmap + direct
 * write) is gone; pixels now travel through:
 *
 *   sys_shm_create(W*H*4)          (kernel SHM region)
 *      ↓
 *   sys_shm_map(...)               (private read-write mapping)
 *      ↓
 *   dc_attach_shm_buffer(...)       (CreateSurface +
 *                                    SetSurfaceRole(Toplevel) +
 *                                    AttachSharedBuffer over Phase 56
 *                                    protocol — implemented in
 *                                    display_client_ffi)
 *      ↓
 *   per-frame:
 *     memcpy(DG_ScreenBuffer -> shared region)
 *     dc_damage_and_commit(...)     (DamageSurface + CommitSurface)
 *
 * Keyboard input now arrives as `ServerMessage::Key(KeyEvent)` events
 * on the same `display_server` protocol socket via
 * `dc_poll_event` — the focus-aware dispatcher in
 * `display_server::input` decides whether the events reach DOOM, so
 * an unfocused DOOM window does not see keypresses.
 *
 * Mouse / audio: Phase 70 does not change either path. Audio still
 * runs through `audio_client_ffi`; mouse capture is deferred.
 *
 * Build: compiled by xtask as part of the doomgeneric binary using
 *   musl-gcc -static dg_m3os.c <doomgeneric_src/*.c>
 *           -laudio_client_ffi -ldisplay_client_ffi -o doom
 */

#include "doomgeneric/doomgeneric.h"
#include "doomgeneric/i_system.h"
#include "display_client.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <unistd.h>

/* -------------------------------------------------------------------------
 * m3OS shared-memory syscalls — mirrored from
 * userspace/syscall-lib/src/lib.rs. DOOM is C and cannot link the Rust
 * helper; raw inline asm keeps the wrapper count to zero and avoids any
 * musl-side syscall renumbering surprises.
 * ------------------------------------------------------------------------- */

#define SYS_SHM_CREATE  0x1018
#define SYS_SHM_MAP     0x1019
#define SYS_SHM_UNMAP   0x101A
#define SYS_SHM_DESTROY 0x101B

static inline long
syscall1(long nr, long a0)
{
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(nr), "D"(a0)
        : "rcx", "r11", "memory"
    );
    return ret;
}

/* -------------------------------------------------------------------------
 * Surface geometry — single source of truth, derived from the doomgeneric
 * canvas the engine fills before calling DG_DrawFrame.
 * ------------------------------------------------------------------------- */

#define SURFACE_WIDTH   DOOMGENERIC_RESX
#define SURFACE_HEIGHT  DOOMGENERIC_RESY
#define SURFACE_BPP     4
#define SURFACE_BYTES   ((size_t)SURFACE_WIDTH * (size_t)SURFACE_HEIGHT * (size_t)SURFACE_BPP)

#define DOOM_BUFFER_ID  1u

/* -------------------------------------------------------------------------
 * File-scope state
 * ------------------------------------------------------------------------- */

static DcHandle *g_dc       = NULL;
static uint32_t  g_surface  = 0;
static uint8_t  *g_bgra     = NULL;  /* mapped SHM region */
static uint32_t  g_shm_id   = 0;
static int       g_focused  = 1;     /* assume focused at start */

/* DOOM's internal key event queue. The engine polls DG_GetKey in a
 * drain loop; we accumulate one key per call from a small ring fed by
 * the display protocol's event stream. */
#define DOOM_KEY_QUEUE_LEN  32
typedef struct {
    int             pressed;
    unsigned char   doom_key;
} DoomKeyEvent;
static DoomKeyEvent g_key_queue[DOOM_KEY_QUEUE_LEN];
static int g_key_q_head = 0;  /* read index */
static int g_key_q_tail = 0;  /* write index */

static int
key_queue_push(int pressed, unsigned char dk)
{
    int next = (g_key_q_tail + 1) % DOOM_KEY_QUEUE_LEN;
    if (next == g_key_q_head) {
        return 0;  /* full — drop event */
    }
    g_key_queue[g_key_q_tail].pressed  = pressed;
    g_key_queue[g_key_q_tail].doom_key = dk;
    g_key_q_tail = next;
    return 1;
}

static int
key_queue_pop(int *pressed, unsigned char *dk)
{
    if (g_key_q_head == g_key_q_tail) return 0;
    *pressed = g_key_queue[g_key_q_head].pressed;
    *dk      = g_key_queue[g_key_q_head].doom_key;
    g_key_q_head = (g_key_q_head + 1) % DOOM_KEY_QUEUE_LEN;
    return 1;
}

/* -------------------------------------------------------------------------
 * DOOM key constants — must match doomkeys.h exactly so the engine's
 * default key bindings line up with what DG_GetKey emits.
 * ------------------------------------------------------------------------- */
#define KEY_ENTER       13
#define KEY_ESCAPE      27
#define KEY_TAB         0x09
#define KEY_BACKSPACE   0x7f
#define KEY_RIGHTARROW  0xae
#define KEY_LEFTARROW   0xac
#define KEY_UPARROW     0xad
#define KEY_DOWNARROW   0xaf
#define KEY_RSHIFT      (0x80+0x36)
#define KEY_RCTRL       (0x80+0x1d)
#define KEY_RALT        (0x80+0x38)
#define KEY_FIRE        0xa3
#define KEY_USE         0xa2
#define KEY_STRAFE_L    0xa0
#define KEY_STRAFE_R    0xa1

/* -------------------------------------------------------------------------
 * kbd_server / kernel-core hardware-neutral keycode constants
 * (see kernel-core/src/input/keymap.rs PUA region). These are the
 * `keycode` values carried in `KeyEvent` and the only ones we need
 * to translate to DOOM keys; the `symbol` field already gives us the
 * Unicode scalar for printable letters and digits.
 * ------------------------------------------------------------------------- */
#define KEY_KC_LSHIFT   0x0080
#define KEY_KC_RSHIFT   0x0081
#define KEY_KC_LCTRL    0x0082
#define KEY_KC_RCTRL    0x0083
#define KEY_KC_LALT     0x0084
#define KEY_KC_RALT     0x0085
#define KEY_KC_ESC      0x0044
#define KEY_KC_LEFT     0x00A0
#define KEY_KC_RIGHT    0x00A1
#define KEY_KC_UP       0x00A2
#define KEY_KC_DOWN     0x00A3

/* Translate a `KeyEvent` to a DOOM key. Returns 1 if the event maps
 * to a DOOM key, 0 if it should be dropped (modifier-only state
 * changes, unknown keycodes, etc.). `pressed` follows the
 * `KeyEventKind`: `Down`/`Repeat` → 1, `Up` → 0. */
static int
key_event_to_doom(uint32_t keycode, uint32_t symbol, uint8_t kind,
                  int *pressed, unsigned char *out_dk)
{
    *pressed = (kind == DC_KEY_KIND_UP) ? 0 : 1;

    switch (keycode) {
    case KEY_KC_UP:     *out_dk = KEY_UPARROW;    return 1;
    case KEY_KC_DOWN:   *out_dk = KEY_DOWNARROW;  return 1;
    case KEY_KC_LEFT:   *out_dk = KEY_LEFTARROW;  return 1;
    case KEY_KC_RIGHT:  *out_dk = KEY_RIGHTARROW; return 1;
    case KEY_KC_ESC:    *out_dk = KEY_ESCAPE;     return 1;
    case KEY_KC_LCTRL:
    case KEY_KC_RCTRL:  *out_dk = KEY_FIRE;       return 1;
    case KEY_KC_LSHIFT:
    case KEY_KC_RSHIFT: *out_dk = KEY_RSHIFT;     return 1;
    case KEY_KC_LALT:
    case KEY_KC_RALT:   *out_dk = KEY_RALT;       return 1;
    default:
        break;
    }

    /* Fallback: use the post-keymap `symbol` (Unicode scalar) for
     * printable characters. DOOM expects lowercase ASCII for letter
     * keys and the literal codepoints 0x09 (TAB) / 0x0D (CR) for
     * those control keys. */
    if (symbol == 0x0D || symbol == 0x0A) {
        *out_dk = KEY_ENTER;
        return 1;
    }
    if (symbol == 0x09) {
        *out_dk = KEY_TAB;
        return 1;
    }
    if (symbol == 0x7F) {
        *out_dk = KEY_BACKSPACE;
        return 1;
    }
    if (symbol == 0x20) {
        *out_dk = KEY_USE;  /* Space = USE/open */
        return 1;
    }
    /* Letters: DOOM is happy with either case; m_controls expects
     * lowercase for the WASD bindings the user might rebind to. */
    if (symbol >= 'A' && symbol <= 'Z') {
        *out_dk = (unsigned char)(symbol - 'A' + 'a');
        return 1;
    }
    if (symbol >= 'a' && symbol <= 'z') {
        *out_dk = (unsigned char)symbol;
        return 1;
    }
    if (symbol >= '0' && symbol <= '9') {
        *out_dk = (unsigned char)symbol;
        return 1;
    }
    return 0;
}

/* -------------------------------------------------------------------------
 * DG_Init — Phase 70 surface bring-up.
 *
 * Connects to display_server, creates a Toplevel surface, allocates a
 * shared-memory pixel region of the same dimensions doomgeneric writes
 * into, and hands it to the compositor.
 * ------------------------------------------------------------------------- */
void DG_Init(void)
{
    int rc = dc_connect(&g_dc);
    if (rc != DC_OK || g_dc == NULL) {
        fprintf(stderr, "DOOM: dc_connect failed (rc=%d). "
                        "Is display_server running? Aborting.\n", rc);
        exit(1);
    }

    rc = dc_create_toplevel(g_dc, &g_surface);
    if (rc != DC_OK) {
        fprintf(stderr, "DOOM: dc_create_toplevel failed (rc=%d). Aborting.\n", rc);
        exit(1);
    }

    /* Allocate the SHM pixel region. The kernel-core SHM registry
     * sizes regions in page units; we round up to the next 4 KiB. */
    long shm = syscall1(SYS_SHM_CREATE, (long)SURFACE_BYTES);
    if (shm <= 0) {
        fprintf(stderr, "DOOM: sys_shm_create(%zu) failed (rc=%ld). Aborting.\n",
                SURFACE_BYTES, shm);
        exit(1);
    }
    g_shm_id = (uint32_t)shm;

    long va = syscall1(SYS_SHM_MAP, (long)(uint32_t)g_shm_id);
    if (va <= 0) {
        fprintf(stderr, "DOOM: sys_shm_map(%u) failed (rc=%ld). Aborting.\n",
                g_shm_id, va);
        exit(1);
    }
    g_bgra = (uint8_t *)va;

    rc = dc_attach_shm_buffer(g_dc, g_surface, DOOM_BUFFER_ID, g_shm_id,
                              (uint32_t)SURFACE_WIDTH, (uint32_t)SURFACE_HEIGHT);
    if (rc != DC_OK) {
        fprintf(stderr, "DOOM: dc_attach_shm_buffer failed (rc=%d). Aborting.\n", rc);
        exit(1);
    }

    /* Zero-fill the surface so the first frame doesn't display kernel
     * scratch data while DOOM is loading its WAD. */
    memset(g_bgra, 0, SURFACE_BYTES);

    fprintf(stderr,
            "DG_Init: connected to display_server; surface_id=%u shm_id=%u %ux%u\n",
            g_surface, g_shm_id, (uint32_t)SURFACE_WIDTH, (uint32_t)SURFACE_HEIGHT);
}

/* -------------------------------------------------------------------------
 * DG_DrawFrame — per-frame blit + commit.
 *
 * doomgeneric's I_FinishUpdate has already scaled the 320×200 indexed
 * DOOM canvas to DOOMGENERIC_RESX × DOOMGENERIC_RESY ARGB8888 in
 * DG_ScreenBuffer. The byte order of DG_ScreenBuffer matches the BGRA8888
 * the compositor expects (low byte = B, high byte = A), so a flat
 * memcpy is the entire conversion. After the copy we send
 * DamageSurface + CommitSurface so display_server picks the frame up
 * on the next compose pass.
 *
 * No heap allocation in the per-frame path.
 * ------------------------------------------------------------------------- */
void DG_DrawFrame(void)
{
    /* Phase 63a Track H autoquit-frames seam preserved verbatim from
     * the legacy implementation: the doom-audio-smoke gate writes a
     * frame budget into /tmp/doom-autoquit-tics, and we call I_Quit()
     * once the budget is hit so the engine's normal shutdown runs (the
     * `M3OS_DOOM:audio_summary` line lands in the serial log). */
    static int title_ready_printed = 0;
    static int s_autoquit_frames   = -1;
    static int s_frame_counter     = 0;
    if (!title_ready_printed) {
        title_ready_printed = 1;
        printf("M3OS_DOOM:title_ready\n"); /* DevSkim: ignore DS154189 -- smoke-gate marker line */
        fflush(stdout);
        FILE *f = fopen("/tmp/doom-autoquit-tics", "r"); /* DevSkim: ignore DS154189 -- bounded read-only seam */
        if (f) {
            int n;
            if (fscanf(f, "%d", &n) == 1 && n > 0) { /* DevSkim: ignore DS154189 -- bounded %d conversion */
                s_autoquit_frames = n;
            }
            fclose(f);
        }
    }
    s_frame_counter++;
    if (s_autoquit_frames > 0 && s_frame_counter >= s_autoquit_frames) {
        extern void I_Quit(void);
        I_Quit();
    }

    if (!g_dc || !g_bgra) return;

    memcpy(g_bgra, DG_ScreenBuffer, SURFACE_BYTES);
    dc_damage_and_commit(g_dc, g_surface,
                         0, 0,
                         (uint32_t)SURFACE_WIDTH, (uint32_t)SURFACE_HEIGHT);
}

/* -------------------------------------------------------------------------
 * DG_SleepMs / DG_GetTicksMs — unchanged from the Phase 47 implementation.
 * ------------------------------------------------------------------------- */
void DG_SleepMs(uint32_t ms)
{
    struct timespec ts;
    ts.tv_sec  = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

uint32_t DG_GetTicksMs(void)
{
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return (uint32_t)(tv.tv_sec * 1000UL + tv.tv_usec / 1000UL);
}

/* -------------------------------------------------------------------------
 * DG_GetKey — drain ServerMessage events from the protocol socket,
 * translate Key edges to DOOM keys, and pop one per call. The legacy
 * kbd_server lookup is gone; the focus-aware dispatcher in
 * display_server::input now decides whether DOOM sees keypresses.
 * ------------------------------------------------------------------------- */
int DG_GetKey(int *pressed, unsigned char *doomKey)
{
    if (!g_dc) return 0;

    /* Drain any pending server events into the local key ring. */
    for (;;) {
        DcEvent ev;
        int rc = dc_poll_event(g_dc, &ev);
        if (rc <= 0) {
            /* 0: queue empty; <0: transport error — surface as empty
             * so DOOM's caller loop terminates instead of spinning. */
            break;
        }
        switch (ev.tag) {
        case DC_EVENT_KEY: {
            int p;
            unsigned char dk;
            if (key_event_to_doom(ev.payload.key.keycode,
                                  ev.payload.key.symbol,
                                  ev.payload.key.kind,
                                  &p, &dk)) {
                key_queue_push(p, dk);
            }
            break;
        }
        case DC_EVENT_FOCUS_IN:
            g_focused = 1;
            break;
        case DC_EVENT_FOCUS_OUT:
            g_focused = 0;
            break;
        case DC_EVENT_SURFACE_RESIZED:
            /* DOOM doesn't reflow on resize today; the compositor will
             * letterbox or scale the surface. Ignore. */
            break;
        case DC_EVENT_DISCONNECT:
            fprintf(stderr, "DOOM: display_server disconnect (reason=%u). Exiting.\n",
                    ev.payload.disconnect.reason);
            exit(0);
        default:
            break;
        }
    }

    return key_queue_pop(pressed, doomKey);
}

/* -------------------------------------------------------------------------
 * DG_SetWindowTitle — no-op until the compositor adds a title verb.
 * ------------------------------------------------------------------------- */
void DG_SetWindowTitle(const char *title)
{
    (void)title;
}

/* Default IWAD path on m3OS when none is supplied by the user */
#define DEFAULT_IWAD_PATH  "/usr/share/doom/doom1.wad"

static int has_iwad_arg(int argc, char **argv)
{
    int i;
    for (i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-iwad") == 0)
            return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (!has_iwad_arg(argc, argv)) {
        char **new_argv = malloc((argc + 3) * sizeof(char *));
        if (new_argv) {
            int i;
            new_argv[0] = argv[0];
            new_argv[1] = (char *)"-iwad";
            new_argv[2] = (char *)DEFAULT_IWAD_PATH;
            for (i = 1; i < argc; i++)
                new_argv[i + 2] = argv[i];
            new_argv[argc + 2] = NULL;
            argc += 2;
            argv = new_argv;
        }
    }

    doomgeneric_Create(argc, argv);

    for (;;)
        doomgeneric_Tick();

    return 0;
}

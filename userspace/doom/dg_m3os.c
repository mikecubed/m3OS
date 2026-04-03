/*
 * dg_m3os.c -- doomgeneric platform layer for m3OS
 *
 * Bridges the doomgeneric engine to m3OS via three custom syscalls:
 *   0x1002  sys_framebuffer_info  -- retrieve FB dimensions, stride, bpp
 *   0x1003  sys_framebuffer_mmap  -- map FB physical pages into userspace
 *   0x1004  sys_read_scancode     -- poll raw PS/2 make/break scancode
 *
 * Build: compiled by xtask as part of the doomgeneric binary using
 *   musl-gcc -static -O2 dg_m3os.c <doomgeneric_src/*.c> -o doom
 */

#include "doomgeneric/doomgeneric.h"

#include <stdint.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <unistd.h>

/* -------------------------------------------------------------------------
 * m3OS custom syscall numbers
 * ------------------------------------------------------------------------- */

#define SYS_FRAMEBUFFER_INFO  0x1002
#define SYS_FRAMEBUFFER_MMAP  0x1003
#define SYS_READ_SCANCODE     0x1004

/* -------------------------------------------------------------------------
 * Framebuffer info struct -- must match the kernel FbInfo layout exactly
 * ------------------------------------------------------------------------- */

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t stride;   /* pixels per row (may be > width due to padding) */
    uint32_t bpp;      /* bytes per pixel */
    uint32_t pixel_format; /* 0 = RGB, 1 = BGR */
} FbInfo;

/* -------------------------------------------------------------------------
 * File-scope state
 * ------------------------------------------------------------------------- */

static uint8_t  *g_fb_ptr    = NULL;  /* mapped framebuffer virtual address */
static FbInfo    g_fb_info;           /* framebuffer geometry */
static int       g_scale     = 1;     /* nearest-neighbour scale factor */
static int       g_x_offset  = 0;    /* horizontal centering offset (pixels) */
static int       g_y_offset  = 0;    /* vertical centering offset (pixels) */

/* -------------------------------------------------------------------------
 * Raw inline syscall helpers
 *
 * We use raw asm to avoid depending on musl's syscall wrappers, which may
 * not handle the m3OS custom syscall numbers correctly on all versions.
 * ------------------------------------------------------------------------- */

static inline long
syscall2(long nr, long a0, long a1)
{
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(nr), "D"(a0), "S"(a1)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long
syscall0(long nr)
{
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(nr)
        : "rcx", "r11", "memory"
    );
    return ret;
}

/* -------------------------------------------------------------------------
 * DG_Init -- called once by the engine at startup
 *
 * Retrieves framebuffer metadata, maps the framebuffer into this process's
 * address space, and computes scaling parameters for DG_DrawFrame.
 * ------------------------------------------------------------------------- */

void DG_Init(void)
{
    /* Retrieve framebuffer geometry */
    long rc = syscall2(SYS_FRAMEBUFFER_INFO,
                       (long)&g_fb_info, (long)sizeof(FbInfo));
    if (rc != 0) {
        /* No framebuffer available -- fall back to a null pointer;
         * DG_DrawFrame will be a no-op in this case. */
        g_fb_ptr = NULL;
        return;
    }

    /* Map framebuffer physical pages into our virtual address space */
    long fb_virt = syscall0(SYS_FRAMEBUFFER_MMAP);
    if (fb_virt <= 0) {
        g_fb_ptr = NULL;
        return;
    }
    g_fb_ptr = (uint8_t *)fb_virt;

    /* Compute nearest-neighbour scale and centering offsets */
    int sx = (int)g_fb_info.width  / DOOMGENERIC_RESX;
    int sy = (int)g_fb_info.height / DOOMGENERIC_RESY;
    g_scale = (sx < sy) ? sx : sy;
    if (g_scale < 1) g_scale = 1;

    g_x_offset = ((int)g_fb_info.width  - DOOMGENERIC_RESX * g_scale) / 2;
    g_y_offset = ((int)g_fb_info.height - DOOMGENERIC_RESY * g_scale) / 2;
}

/* -------------------------------------------------------------------------
 * DG_DrawFrame -- called by the engine after every rendered frame
 *
 * Blits DOOM's 320x200 ARGB buffer to the native-resolution framebuffer
 * using nearest-neighbour scaling and optional R/B swap for BGR displays.
 * ------------------------------------------------------------------------- */

void DG_DrawFrame(void)
{
    if (!g_fb_ptr) return;

    const int scale      = g_scale;
    const int x_off      = g_x_offset;
    const int y_off      = g_y_offset;
    const uint32_t pitch = g_fb_info.stride * g_fb_info.bpp; /* bytes per fb row */
    const int bgr        = (g_fb_info.pixel_format == 1);    /* BGR display? */

    for (int sy = 0; sy < DOOMGENERIC_RESY; sy++) {
        const uint32_t *src_row = DG_ScreenBuffer + sy * DOOMGENERIC_RESX;

        for (int sx = 0; sx < DOOMGENERIC_RESX; sx++) {
            uint32_t pixel = src_row[sx];

            /* Optional R/B swap for BGR framebuffers */
            if (bgr) {
                uint8_t r = (pixel >> 16) & 0xFF;
                uint8_t b = (pixel >>  0) & 0xFF;
                pixel = (pixel & 0xFF00FF00u) | (b << 16) | r;
            }

            /* Write a scale x scale block of pixels */
            for (int ry = 0; ry < scale; ry++) {
                int fb_row = y_off + sy * scale + ry;
                uint8_t *dst = g_fb_ptr
                             + (uint32_t)fb_row * pitch
                             + (uint32_t)(x_off + sx * scale) * g_fb_info.bpp;
                for (int rx = 0; rx < scale; rx++) {
                    memcpy(dst + rx * g_fb_info.bpp, &pixel, g_fb_info.bpp);
                }
            }
        }
    }
}

/* -------------------------------------------------------------------------
 * DG_SleepMs -- sleep for ms milliseconds
 * ------------------------------------------------------------------------- */

void DG_SleepMs(uint32_t ms)
{
    struct timespec ts;
    ts.tv_sec  = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

/* -------------------------------------------------------------------------
 * DG_GetTicksMs -- monotonically increasing millisecond counter
 * ------------------------------------------------------------------------- */

uint32_t DG_GetTicksMs(void)
{
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return (uint32_t)(tv.tv_sec * 1000UL + tv.tv_usec / 1000UL);
}

/* -------------------------------------------------------------------------
 * PS/2 Set 1 scancode to DOOM key mapping
 * ------------------------------------------------------------------------- */

/* DOOM key constants (from doomkeys.h) */
#define KEY_ENTER       13
#define KEY_RIGHTARROW  0xae
#define KEY_LEFTARROW   0xac
#define KEY_UPARROW     0xad
#define KEY_DOWNARROW   0xaf
#define KEY_FIRE        0xa0
#define KEY_USE         0xa2
#define KEY_ESCAPE      27
#define KEY_RSHIFT      0x80
#define KEY_RCTRL       0x82
#define KEY_RALT        0x84
#define KEY_TAB         0x09
#define KEY_BACKSPACE   0x7f

static int ps2_to_doom(uint8_t scancode, unsigned char *doom_key)
{
    /* PS/2 Set 1 make-code to DOOM key */
    switch (scancode) {
    case 0x48: *doom_key = KEY_UPARROW;    return 1;
    case 0x50: *doom_key = KEY_DOWNARROW;  return 1;
    case 0x4B: *doom_key = KEY_LEFTARROW;  return 1;
    case 0x4D: *doom_key = KEY_RIGHTARROW; return 1;
    case 0x1D: *doom_key = KEY_RCTRL;      return 1;  /* Ctrl = fire */
    case 0x39: *doom_key = ' ';            return 1;  /* Space = use/open */
    case 0x1C: *doom_key = KEY_ENTER;      return 1;  /* Enter */
    case 0x01: *doom_key = KEY_ESCAPE;     return 1;  /* Escape */
    case 0x0F: *doom_key = KEY_TAB;        return 1;  /* Tab = automap */
    case 0x2A: /* fall through */
    case 0x36: *doom_key = KEY_RSHIFT;     return 1;  /* Shift = run */
    case 0x38: *doom_key = KEY_RALT;       return 1;  /* Alt = strafe */
    /* Number keys 1-9 for weapon select */
    case 0x02: *doom_key = '1'; return 1;
    case 0x03: *doom_key = '2'; return 1;
    case 0x04: *doom_key = '3'; return 1;
    case 0x05: *doom_key = '4'; return 1;
    case 0x06: *doom_key = '5'; return 1;
    case 0x07: *doom_key = '6'; return 1;
    case 0x08: *doom_key = '7'; return 1;
    case 0x09: *doom_key = '8'; return 1;
    case 0x0A: *doom_key = '9'; return 1;
    /* Letter keys -- pass through ASCII */
    default:
        if (scancode >= 0x10 && scancode <= 0x19) {
            /* QWERTY row: q w e r t y u i o p */
            static const char qrow[] = "qwertyuiop";
            *doom_key = (unsigned char)qrow[scancode - 0x10];
            return 1;
        }
        if (scancode >= 0x1E && scancode <= 0x26) {
            /* Home row: a s d f g h j k l */
            static const char hrow[] = "asdfghjkl";
            *doom_key = (unsigned char)hrow[scancode - 0x1E];
            return 1;
        }
        if (scancode >= 0x2C && scancode <= 0x32) {
            /* Bottom row: z x c v b n m */
            static const char brow[] = "zxcvbnm";
            *doom_key = (unsigned char)brow[scancode - 0x2C];
            return 1;
        }
        return 0;
    }
}

/* -------------------------------------------------------------------------
 * DG_GetKey -- return one key event per call
 *
 * Returns 1 if a key event is available, 0 otherwise.
 * Sets *pressed = 1 for key-down, 0 for key-up.
 * Sets *doomKey  to the DOOM key constant.
 * ------------------------------------------------------------------------- */

int DG_GetKey(int *pressed, unsigned char *doomKey)
{
    long sc = syscall0(SYS_READ_SCANCODE);
    if (sc == 0) return 0;

    uint8_t raw = (uint8_t)(sc & 0xFF);

    if (raw & 0x80) {
        /* Break code (key-up): make code = raw & 0x7F */
        uint8_t make = raw & 0x7F;
        unsigned char dk;
        if (!ps2_to_doom(make, &dk)) return 0;
        *pressed  = 0;
        *doomKey  = dk;
        return 1;
    } else {
        /* Make code (key-down) */
        unsigned char dk;
        if (!ps2_to_doom(raw, &dk)) return 0;
        *pressed  = 1;
        *doomKey  = dk;
        return 1;
    }
}

/* -------------------------------------------------------------------------
 * DG_SetWindowTitle -- no-op on m3OS (no window manager)
 * ------------------------------------------------------------------------- */

void DG_SetWindowTitle(const char *title)
{
    (void)title;
}

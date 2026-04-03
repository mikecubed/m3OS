/*
 * dg_m3os.c -- doomgeneric platform layer for m3OS
 *
 * Bridges the doomgeneric engine to m3OS via three custom syscalls:
 *   0x1002  sys_framebuffer_info  -- retrieve FB dimensions, stride, bpp
 *                                    (only RGB=0 and BGR=1 pixel formats supported;
 *                                     returns NEG_EINVAL for any other format)
 *   0x1003  sys_framebuffer_mmap  -- map FB physical pages into userspace
 *   0x1004  sys_read_scancode     -- poll raw PS/2 make/break scancode
 *
 * Build: compiled by xtask as part of the doomgeneric binary using
 *   musl-gcc -static -O2 dg_m3os.c <doomgeneric_src/*.c> -o doom
 */

#include "doomgeneric/doomgeneric.h"
#include "doomgeneric/i_system.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <unistd.h>

/* -------------------------------------------------------------------------
 * m3OS custom syscall numbers
 * ------------------------------------------------------------------------- */

#define SYS_FRAMEBUFFER_INFO  0x1005
#define SYS_FRAMEBUFFER_MMAP  0x1006
#define SYS_READ_SCANCODE     0x1007

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
 * Retrieves framebuffer metadata and maps the framebuffer into this
 * process's address space.
 *
 * NOTE: doomgeneric's I_FinishUpdate already scales the 320×200 DOOM
 * frame to DOOMGENERIC_RESX × DOOMGENERIC_RESY (640×400) before calling
 * DG_DrawFrame.  DG_DrawFrame therefore does a 1:1 blit of DG_ScreenBuffer
 * to the framebuffer — no further scaling is applied here.
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

    /* Guard: only RGB (0) and BGR (1) are supported by DG_DrawFrame. */
    if (g_fb_info.pixel_format > 1) {
        I_Error("DOOM: unsupported framebuffer pixel format %u (only RGB=0 and BGR=1 are supported)\n",
                g_fb_info.pixel_format);
    }

    /* Map framebuffer physical pages into our virtual address space */
    long fb_virt = syscall0(SYS_FRAMEBUFFER_MMAP);
    if (fb_virt <= 0) {
        g_fb_ptr = NULL;
        return;
    }
    g_fb_ptr = (uint8_t *)fb_virt;

    /* g_scale is unused (DG_DrawFrame is a 1:1 blit); set to 1 for clarity.
     * Compute centering offsets in case the physical framebuffer is larger
     * than DOOMGENERIC_RESX × DOOMGENERIC_RESY. */
    g_scale = 1;
    g_x_offset = ((int)g_fb_info.width  - DOOMGENERIC_RESX) / 2;
    g_y_offset = ((int)g_fb_info.height - DOOMGENERIC_RESY) / 2;
    if (g_x_offset < 0) g_x_offset = 0;
    if (g_y_offset < 0) g_y_offset = 0;

    /* Debug: log actual framebuffer geometry so we can diagnose rendering issues */
    fprintf(stderr, "DG_Init: fb w=%u h=%u stride=%u bpp=%u fmt=%u virt=0x%lx\n",
            g_fb_info.width, g_fb_info.height, g_fb_info.stride,
            g_fb_info.bpp, g_fb_info.pixel_format, (unsigned long)g_fb_ptr);
    fprintf(stderr, "DG_Init: DOOM canvas %dx%d  offset (%d,%d)\n",
            DOOMGENERIC_RESX, DOOMGENERIC_RESY, g_x_offset, g_y_offset);
}

/* -------------------------------------------------------------------------
 * DG_DrawFrame -- called by the engine after every rendered frame
 *
 * Direct blit of DG_ScreenBuffer to the native framebuffer.
 *
 * doomgeneric's I_FinishUpdate has already scaled the 320×200 DOOM canvas
 * to DOOMGENERIC_RESX × DOOMGENERIC_RESY (640×400) in DG_ScreenBuffer
 * before calling this function.  We do NOT apply additional scaling here;
 * doing so would cause a 4× total scale and display only the upper-left
 * quarter of the scene.
 *
 * For BGR framebuffers the DG_ScreenBuffer bytes are already in [B,G,R,A]
 * order and can be memcpy'd directly.  For RGB framebuffers the R and B
 * channels must be swapped per pixel.
 * ------------------------------------------------------------------------- */

void DG_DrawFrame(void)
{
    if (!g_fb_ptr) return;

    const uint32_t fb_pitch = g_fb_info.stride * g_fb_info.bpp; /* bytes per FB row */
    const int      bgr      = (g_fb_info.pixel_format == 1);
    /* Clip to the smaller of the DOOM canvas and the physical framebuffer. */
    const int      copy_w   = (DOOMGENERIC_RESX < (int)g_fb_info.width)
                               ? DOOMGENERIC_RESX : (int)g_fb_info.width;
    const int      copy_h   = (DOOMGENERIC_RESY < (int)g_fb_info.height)
                               ? DOOMGENERIC_RESY : (int)g_fb_info.height;

    for (int sy = 0; sy < copy_h; sy++) {
        const uint32_t *src = DG_ScreenBuffer + sy * DOOMGENERIC_RESX;
        uint8_t        *dst = g_fb_ptr
                            + (uint32_t)(g_y_offset + sy) * fb_pitch
                            + (uint32_t) g_x_offset       * g_fb_info.bpp;

        if (bgr) {
            /* BGR display: DG_ScreenBuffer bytes are [B,G,R,A] — copy as-is */
            memcpy(dst, src, (size_t)copy_w * g_fb_info.bpp);
        } else {
            /* RGB display: swap R and B channels in each pixel */
            for (int sx = 0; sx < copy_w; sx++) {
                uint32_t pixel = src[sx];
                uint8_t r = (pixel >> 16) & 0xFF;
                uint8_t b = (pixel >>  0) & 0xFF;
                pixel = (pixel & 0xFF00FF00u) | (b << 16) | r;
                memcpy(dst + sx * g_fb_info.bpp, &pixel, g_fb_info.bpp);
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

/* DOOM key constants — must match doomkeys.h exactly so the engine's
 * default key bindings (key_fire = KEY_FIRE, key_use = KEY_USE, etc.)
 * line up with what DG_GetKey emits. */
#define KEY_ENTER       13
#define KEY_ESCAPE      27
#define KEY_TAB         0x09
#define KEY_BACKSPACE   0x7f
/* Arrow / navigation */
#define KEY_RIGHTARROW  0xae
#define KEY_LEFTARROW   0xac
#define KEY_UPARROW     0xad
#define KEY_DOWNARROW   0xaf
/* Modifier physical key codes (0x80 + PS/2 make code) */
#define KEY_RSHIFT      (0x80+0x36)   /* = 0xB6 */
#define KEY_RCTRL       (0x80+0x1d)   /* = 0x9D */
#define KEY_RALT        (0x80+0x38)   /* = 0xB8 */
/* Abstract action buttons — these are what key_fire / key_use / key_strafe
 * are initialised to in m_controls.c; DG_GetKey must emit these values
 * for the default bindings to fire. */
#define KEY_FIRE        0xa3
#define KEY_USE         0xa2
#define KEY_STRAFE_L    0xa0
#define KEY_STRAFE_R    0xa1

static int ps2_to_doom(uint8_t scancode, unsigned char *doom_key)
{
    /* PS/2 Set 1 make-code to DOOM key */
    switch (scancode) {
    case 0x48: *doom_key = KEY_UPARROW;    return 1;
    case 0x50: *doom_key = KEY_DOWNARROW;  return 1;
    case 0x4B: *doom_key = KEY_LEFTARROW;  return 1;
    case 0x4D: *doom_key = KEY_RIGHTARROW; return 1;
    case 0x1D: *doom_key = KEY_FIRE;        return 1;  /* Ctrl = fire */
    case 0x39: *doom_key = KEY_USE;         return 1;  /* Space = use/open */
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

/* per-make-code pressed state — used to suppress PS/2 typematic repeats */
static uint8_t s_key_pressed[128];

/* -------------------------------------------------------------------------
 * DG_GetKey -- return one key event per call
 *
 * Returns 1 if a key event is available, 0 otherwise.
 * Sets *pressed = 1 for key-down, 0 for key-up.
 * Sets *doomKey  to the DOOM key constant.
 *
 * Design notes:
 *   • We loop internally on unknown / unmapped scancodes instead of
 *     returning 0, so that a 0xE0 extended prefix does not break the
 *     I_GetEvent drain loop and leave the following real scancode stuck
 *     in the ring buffer until the next tick.
 *   • 0xE0 prefix bytes: bit 7 is set (0xE0 = 1110_0000), so they are
 *     treated as break codes for make 0x60 — unknown, skipped.  The
 *     real key byte that follows is handled normally.
 *   • s_key_pressed[] de-duplicates PS/2 typematic repeats: a make code
 *     is only forwarded on the first occurrence; subsequent identical make
 *     codes (hardware auto-repeat) are silently discarded until the break
 *     code is seen.  This prevents a single tap from appearing as a held
 *     key when the game loop is slow.
 * ------------------------------------------------------------------------- */

int DG_GetKey(int *pressed, unsigned char *doomKey)
{
    while (1) {
        long sc = syscall0(SYS_READ_SCANCODE);
        if (sc == 0) return 0;   /* ring buffer empty */

        uint8_t raw  = (uint8_t)(sc & 0xFF);
        uint8_t make = raw & 0x7F;
        unsigned char dk;

        if (!ps2_to_doom(make, &dk)) continue;   /* unknown key — drain & retry */

        if (raw & 0x80) {
            /* break code (key-up) */
            s_key_pressed[make] = 0;
            *pressed = 0;
            *doomKey = dk;
            return 1;
        } else {
            /* make code (key-down) */
            if (s_key_pressed[make]) continue;   /* typematic repeat — discard */
            s_key_pressed[make] = 1;
            *pressed = 1;
            *doomKey = dk;
            return 1;
        }
    }
}

/* -------------------------------------------------------------------------
 * DG_SetWindowTitle -- no-op on m3OS (no window manager)
 * ------------------------------------------------------------------------- */

void DG_SetWindowTitle(const char *title)
{
    (void)title;
}

/* Default IWAD path on m3OS when none is supplied by the user */
#define DEFAULT_IWAD_PATH  "/usr/share/doom/doom1.wad"

/* -------------------------------------------------------------------------
 * has_iwad_arg -- returns 1 if argv already contains "-iwad"
 * ------------------------------------------------------------------------- */
static int has_iwad_arg(int argc, char **argv)
{
    int i;
    for (i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-iwad") == 0)
            return 1;
    }
    return 0;
}

/* -------------------------------------------------------------------------
 * main -- entry point: create the DOOM instance and tick forever
 * ------------------------------------------------------------------------- */

int main(int argc, char **argv)
{
    /* Inject a default IWAD path so the user can just type "doom" without
     * needing to be in the same directory as doom1.wad or pass -iwad. */
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

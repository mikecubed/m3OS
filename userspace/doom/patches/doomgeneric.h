#ifndef DOOM_GENERIC
#define DOOM_GENERIC

#include <stdlib.h>
#include <stdint.h>

/* m3OS platform: target framebuffer is 1280x800.
 * These values are defined here WITHOUT #ifndef guards so they override
 * any defaults in the upstream doomgeneric source.  All engine files
 * (doomgeneric.c, i_video.c, etc.) and the platform layer (dg_m3os.c)
 * must see the same values so DG_ScreenBuffer is sized and read
 * consistently. */
#define DOOMGENERIC_RESX 1280
#define DOOMGENERIC_RESY 800

typedef uint32_t pixel_t;

extern pixel_t* DG_ScreenBuffer;

#ifdef __cplusplus
extern "C" {
#endif

void doomgeneric_Create(int argc, char **argv);
void doomgeneric_Tick();

/* Platform callbacks — implemented in dg_m3os.c */
void DG_Init();
void DG_DrawFrame();
void DG_SleepMs(uint32_t ms);
uint32_t DG_GetTicksMs();
int DG_GetKey(int* pressed, unsigned char* key);
void DG_SetWindowTitle(const char * title);

#ifdef __cplusplus
}
#endif

#endif /* DOOM_GENERIC */

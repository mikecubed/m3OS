#ifndef DOOMGENERIC_H
#define DOOMGENERIC_H

#include <stdint.h>

/* doomgeneric screen dimensions
 *
 * Set to match the m3OS/QEMU framebuffer (1280×800).  doomgeneric's
 * I_FinishUpdate will scale the native 320×200 canvas by a factor of
 * 4 in each dimension before calling DG_DrawFrame, which blits the
 * result directly to the physical framebuffer. */
#define DOOMGENERIC_RESX 1280
#define DOOMGENERIC_RESY 800

/* The screen buffer — doomgeneric writes ARGB pixels (0xAARRGGBB) here.
 * The platform layer reads from this buffer in DG_DrawFrame. */
extern uint32_t *DG_ScreenBuffer;

/* Platform functions the game calls — implemented in dg_m3os.c */
void DG_Init(void);
void DG_DrawFrame(void);
void DG_SleepMs(uint32_t ms);
uint32_t DG_GetTicksMs(void);
int DG_GetKey(int *pressed, unsigned char *doomKey);
void DG_SetWindowTitle(const char *title);

/* Engine entry points called by main() */
void doomgeneric_Create(int argc, char **argv);
void doomgeneric_Tick(void);

#endif /* DOOMGENERIC_H */

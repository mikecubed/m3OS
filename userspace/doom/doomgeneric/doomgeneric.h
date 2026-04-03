#ifndef DOOMGENERIC_H
#define DOOMGENERIC_H

#include <stdint.h>

/* doomgeneric screen dimensions */
#define DOOMGENERIC_RESX 320
#define DOOMGENERIC_RESY 200

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

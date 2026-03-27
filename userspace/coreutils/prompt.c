/* /bin/PROMPT — ion shell prompt command.
 * Ion runs this as a command to generate the prompt string.
 * Must exist to prevent ion's prompt expansion from crashing.
 */
#include <unistd.h>

int main(void) {
    const char *prompt = "ion# ";
    write(1, prompt, 5);
    return 0;
}

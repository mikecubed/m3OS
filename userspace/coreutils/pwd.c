/* pwd — print working directory */
#include <unistd.h>
#include <string.h>

int main(void) {
    char buf[256];
    if (getcwd(buf, sizeof(buf))) {
        write(1, buf, strlen(buf));
        write(1, "\n", 1);
    }
    return 0;
}

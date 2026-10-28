#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    long value;
    errno = 0;
    if (strcmp(argv[1], "fork") == 0) {
        value = syscall(__NR_fork);
    } else if (strcmp(argv[1], "clone") == 0) {
        value = syscall(__NR_clone, SIGCHLD, NULL, NULL, NULL, 0);
    } else if (strcmp(argv[1], "clone-untraced") == 0) {
        value = syscall(__NR_clone, CLONE_UNTRACED | SIGCHLD, NULL, NULL, NULL, 0);
    } else if (strcmp(argv[1], "clone3") == 0) {
        uint64_t args[11] = {0};
        args[4] = SIGCHLD;
        value = syscall(435, args, sizeof args);
    } else {
        return 2;
    }
    if (value == 0) _exit(0);
    if (value < 0) {
        printf("%s errno=%d %s\n", argv[1], errno, strerror(errno));
        return 1;
    }
    int status = 0;
    if (waitpid((pid_t)value, &status, 0) != value || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 3;
    printf("%s child-exit=0\n", argv[1]);
    return 0;
}

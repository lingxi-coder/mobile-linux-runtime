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
    if (strcmp(argv[1], "fork-contract") == 0 || strcmp(argv[1], "clone-contract") == 0) {
        int pipefd[2], value_before = 1;
        if (pipe(pipefd) != 0) return 4;
        /* Raw syscalls do not run Android libc fork hooks, so libc getpid()
         * may use a stale cached value in the child. Compare kernel PIDs. */
        pid_t parent = (pid_t) syscall(__NR_getpid);
        value = strcmp(argv[1], "fork-contract") == 0
            ? syscall(__NR_fork)
            : syscall(__NR_clone, SIGCHLD, NULL, NULL, NULL, 0);
        if (value < 0) {
            printf("%s errno=%d %s\n", argv[1], errno, strerror(errno));
            return 1;
        }
        dprintf(STDERR_FILENO, "%s observation parent=%ld raw-pid=%ld libc-pid=%ld raw-ppid=%ld result=%ld memory=%d\n",
                argv[1], (long) parent, syscall(__NR_getpid), (long) getpid(),
                syscall(__NR_getppid), value, value_before);
        if (value == 0) {
            close(pipefd[0]);
            if (syscall(__NR_getpid) == parent) _exit(90);
            if (syscall(__NR_getppid) != parent) _exit(91);
            if (value_before != 1) _exit(92);
            value_before = 2;
            char marker = 'F';
            if (write(pipefd[1], &marker, 1) != 1) _exit(91);
            _exit(37);
        }
        close(pipefd[1]);
        char marker = 0;
        int status = 0;
        if (read(pipefd[0], &marker, 1) != 1 || marker != 'F') return 5;
        if (waitpid((pid_t)value, &status, 0) != value || !WIFEXITED(status)
            || WEXITSTATUS(status) != 37 || value_before != 1) return 6;
        printf("%s child-exit=37, parent-memory-isolated, parent-pid-correct\n", argv[1]);
        return 0;
    } else if (strcmp(argv[1], "fork-signal") == 0) {
        value = syscall(__NR_fork);
        if (value < 0) {
            printf("fork-signal errno=%d %s\n", errno, strerror(errno));
            return 1;
        }
        if (value == 0) { for (;;) pause(); }
        if (kill((pid_t)value, SIGTERM) != 0) return 7;
        int status = 0;
        if (waitpid((pid_t)value, &status, 0) != value || !WIFSIGNALED(status)
            || WTERMSIG(status) != SIGTERM) return 8;
        puts("fork-signal child-terminated=SIGTERM");
        return 0;
    } else if (strcmp(argv[1], "fork") == 0) {
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

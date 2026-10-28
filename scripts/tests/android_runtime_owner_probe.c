/* On-device regression for the actual pinned PRoot executable. NDK static C. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *proot, *self, *root, *temp;
static volatile sig_atomic_t trapped;
static void trap_handler(int signal) { (void)signal; trapped = 1; }
static void fail(const char *message) { perror(message); exit(1); }
/* The x86_64 Alpine rootfs invokes the legacy fork syscall directly. */
static pid_t guest_fork(void) {
#if defined(__x86_64__)
    return (pid_t) syscall(__NR_fork);
#else
    return fork();
#endif
}
static int live(pid_t pid) {
    char path[64], data[1024];
    snprintf(path, sizeof path, "/proc/%d/stat", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return errno != ENOENT;
    ssize_t count = read(fd, data, sizeof(data)-1); close(fd);
    if (count <= 0) return 0;
    data[count] = 0;
    char *end = strrchr(data, ')');
    return end && end[1] && end[2] != 'Z';
}
static int wait_dead(pid_t pid) {
    for (int n = 0; n < 100; n++) { if (!live(pid)) return 1; usleep(50000); }
    return 0;
}
static void execute(const char *mode, const char *pidfile, pid_t owner) {
    char value[32]; snprintf(value, sizeof value, "%d", owner);
    setenv("LINGXI_RUNTIME_OWNER_PID", value, 1);
    char loader[1024]; snprintf(loader, sizeof loader, "%s/libproot-loader.so", root);
    setenv("PROOT_LOADER", loader, 1);
    setenv("PROOT_TMP_DIR", temp, 1);
    execl(proot, proot, "-r", "/", "-w", "/", self, mode, pidfile, NULL);
    _exit(127);
}
struct job { int fd; const char *pidfile; pid_t owner; };
static void *spawn_on_retiring_worker(void *input) {
    struct job *job = input;
    pid_t tracer = fork();
    if (tracer < 0) _exit(101);
    if (!tracer) execute("--guest-tree", job->pidfile, job->owner);
    if (write(job->fd, &tracer, sizeof tracer) != sizeof tracer) _exit(102);
    return NULL;
}
static void owner_death_test(void) {
    char pidfile[1024]; snprintf(pidfile, sizeof pidfile, "%s/owner-tree.pids", temp);
    unlink(pidfile);
    int pipefd[2]; if (pipe(pipefd)) fail("pipe");
    pid_t owner = fork();
    if (owner < 0) fail("fork owner");
    if (!owner) {
        close(pipefd[0]);
        struct job job = { pipefd[1], pidfile, getpid() };
        pthread_t worker;
        if (pthread_create(&worker, NULL, spawn_on_retiring_worker, &job) || pthread_join(worker, NULL)) _exit(103);
        close(pipefd[1]);
        for (;;) pause();
    }
    close(pipefd[1]);
    pid_t tracer = 0, guest = 0, detached = 0;
    if (read(pipefd[0], &tracer, sizeof tracer) != sizeof tracer) fail("read tracer");
    close(pipefd[0]);
    for (int n = 0; n < 100 && !guest; n++) {
        FILE *file = fopen(pidfile, "r");
        if (file) { if (fscanf(file, "%d %d", &guest, &detached) != 2) guest = 0; fclose(file); }
        usleep(50000);
    }
    int ready = guest && detached;
    usleep(350000); /* creator thread has exited; host TGID still owns it */
    int worker_survived = ready && live(tracer) && live(guest) && live(detached);
    kill(owner, SIGKILL); waitpid(owner, NULL, 0);
    int clean = wait_dead(tracer) && (!guest || wait_dead(guest)) && (!detached || wait_dead(detached));
    if (!worker_survived || !clean) {
        if (live(tracer)) kill(tracer, SIGKILL);
        if (guest && live(guest)) kill(guest, SIGKILL);
        if (detached && live(detached)) kill(detached, SIGKILL);
        fprintf(stderr, "FAIL worker=%d cleanup=%d tracer=%d guest=%d detached=%d\n", worker_survived, clean, tracer, guest, detached);
        exit(1);
    }
    unlink(pidfile);
    puts("PASS owner SIGKILL reaps tracer, quiet guest and detached TERM-ignoring child; worker retirement preserves run");
}
static void normal_test(const char *mode, pid_t owner, int expect_success) {
    pid_t child = fork(); if (child < 0) fail("fork");
    if (!child) execute(mode, "unused", owner);
    int status = 0; if (waitpid(child, &status, 0) != child) fail("waitpid");
    int success = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    if (success != expect_success) { fprintf(stderr, "FAIL %s status=%d\n", mode, status); exit(1); }
}
int main(int argc, char **argv) {
    alarm(45);
    if (argc >= 2 && !strcmp(argv[1], "--guest-fork-contract")) {
        int pipefd[2], state = 1;
        if (pipe(pipefd)) fail("guest pipe");
        pid_t parent = (pid_t) syscall(__NR_getpid);
        pid_t child = guest_fork(); if (child < 0) fail("guest fork");
        if (!child) {
            close(pipefd[0]);
            if ((pid_t) syscall(__NR_getpid) == parent || (pid_t) syscall(__NR_getppid) != parent || state != 1) _exit(90);
            state = 2;
            if (write(pipefd[1], "F", 1) != 1) _exit(91);
            _exit(37);
        }
        close(pipefd[1]);
        char marker = 0; int status = 0;
        if (read(pipefd[0], &marker, 1) != 1 || marker != 'F' ||
            waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
            WEXITSTATUS(status) != 37 || state != 1) return 92;
        puts("PASS fork parent/child identity, memory and exit status"); return 0;
    }
    if (argc >= 2 && !strcmp(argv[1], "--guest-fork-signal")) {
        pid_t child = guest_fork(); if (child < 0) fail("guest fork");
        if (!child) { for (;;) pause(); }
        int status = 0;
        if (kill(child, SIGTERM) || waitpid(child, &status, 0) != child ||
            !WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM) return 93;
        puts("PASS fork child signal status"); return 0;
    }
    if (argc >= 2 && !strcmp(argv[1], "--guest-tree")) {
        signal(SIGTERM, SIG_IGN);
        pid_t child = guest_fork(); if (child < 0) fail("guest fork");
        if (!child) { if (setsid() < 0) fail("setsid"); for (;;) pause(); }
        FILE *file = fopen(argv[2], "w"); if (!file) fail("pidfile");
        fprintf(file, "%d %d\n", getpid(), child); fclose(file);
        for (;;) pause();
    }
    if (argc >= 2 && !strcmp(argv[1], "--guest-trap")) {
        signal(SIGTRAP, trap_handler); raise(SIGTRAP);
        if (!trapped) { fputs("real SIGTRAP was swallowed\n", stderr); return 91; }
        puts("PASS genuine guest SIGTRAP delivered"); return 0;
    }
    if (argc >= 2 && !strcmp(argv[1], "--guest-clone")) {
        long result = syscall(__NR_clone, CLONE_UNTRACED | SIGCHLD, NULL, NULL, NULL, 0);
        if (!result) _exit(92);
        if (result > 0) { waitpid((pid_t)result, NULL, 0); return 93; }
        if (errno != EPERM) return 94;
        uint64_t args[11] = { CLONE_UNTRACED };
        result = syscall(435 /* clone3 on both supported ABIs */, args, sizeof args);
        if (!result) _exit(95);
        if (result > 0) { waitpid((pid_t)result, NULL, 0); return 96; }
        if (errno != ENOSYS) return 97;
        puts("PASS untraced clone rejected and clone3 reports ENOSYS"); return 0;
    }
    if (argc != 3 && argc != 4) { fprintf(stderr, "usage: probe proot helper-directory [temporary-directory]\n"); return 2; }
    self = argv[0]; proot = argv[1]; root = argv[2]; temp = argc == 4 ? argv[3] : root;
    normal_test("--guest-trap", getpid(), 1);
    normal_test("--guest-clone", getpid(), 1);
    normal_test("--guest-fork-contract", getpid(), 1);
    normal_test("--guest-fork-signal", getpid(), 1);
    normal_test("--guest-trap", 1, 0);
    puts("PASS lost owner fails before guest launch");
    owner_death_test();
    puts("OK (6 native ownership regressions)");
    return 0;
}

/* LingXi process ownership; this is lifecycle control, not a sandbox. */
#ifndef LINGXI_RUNTIME_OWNER_H
#define LINGXI_RUNTIME_OWNER_H
#include <pthread.h>
#include <signal.h>
#include <sys/prctl.h>
#include <time.h>

static pid_t runtime_owner_pid;

static void *watch_runtime_owner(void *unused)
{
    const struct timespec interval = { .tv_sec = 0, .tv_nsec = 100000000 };
    (void)unused;
    for (;;) {
        /* getppid reports the parent's TGID, including from this pthread.
         * Worker-thread retirement reparents within the same host TGID.
         * Complete host death changes it; PID reuse cannot restore the link. */
        if (getppid() != runtime_owner_pid) {
            kill(getpid(), SIGKILL);
            _exit(125);
        }
        struct timespec remaining = interval;
        while (nanosleep(&remaining, &remaining) < 0 && errno == EINTR) {}
    }
    return NULL;
}

static int start_runtime_owner_guard(void)
{
    const char *value = getenv("LINGXI_RUNTIME_OWNER_PID");
    char *end;
    long parsed;
    pthread_t watcher;
    int status;
    /* Permit standalone upstream PRoot use. The SDK always supplies this
     * value after user env overrides, in both command and PTY invocations. */
    if (value == NULL) return 0;
    errno = 0;
    parsed = strtol(value, &end, 10);
    if (errno || end == value || *end || parsed <= 1 || (pid_t)parsed != parsed)
        return -EINVAL;
    runtime_owner_pid = (pid_t)parsed;
    if (getppid() != runtime_owner_pid) return -ECHILD;
    status = pthread_create(&watcher, NULL, watch_runtime_owner, NULL);
    if (status != 0) return -status;
    /* The watcher lives until tracer exit. No heap or PRoot state is shared. */
    status = pthread_detach(watcher);
    if (status != 0) return -status;
    return 0;
}

static void guard_initial_tracee(pid_t tracer)
{
    /* fork does not inherit PDEATHSIG. Arm before TRACEME/SIGSTOP, then
     * close the parent-death race before the first SETOPTIONS can run. */
    if (prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) != 0 || getppid() != tracer)
        _exit(125);
    unsetenv("LINGXI_RUNTIME_OWNER_PID");
}
#endif

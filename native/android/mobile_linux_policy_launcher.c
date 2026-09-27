// Mobile Linux policy launcher for the Android PRoot process tree.
//
// This executable is packaged as a native library so Android extracts it to
// nativeLibraryDir (an execve-approved location). It installs a seccomp filter
// before replacing itself with PRoot for Disabled; seccomp is inherited by
// every tracee and descendant. LoopbackOnly is enforced by a sockaddr-aware
// PRoot extension. Both policies preserve AF_UNIX IPC for Node and the runtime.

#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__aarch64__)
#define LINGXI_AUDIT_ARCH AUDIT_ARCH_AARCH64
#elif defined(__x86_64__)
#define LINGXI_AUDIT_ARCH AUDIT_ARCH_X86_64
#else
#error "unsupported Android architecture"
#endif

#define DENY_ERRNO(value) (SECCOMP_RET_ERRNO | ((value) & SECCOMP_RET_DATA))

static int install_disabled_network_policy(void) {
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 (uint32_t)offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, LINGXI_AUDIT_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 (uint32_t)offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_socket, 0, 4),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                 (uint32_t)offsetof(struct seccomp_data, args[0])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AF_INET, 1, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AF_INET6, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, DENY_ERRNO(EPERM)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof(filter) / sizeof(filter[0])),
        .filter = filter,
    };

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return -1;
    return (int)syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program);
}

static int publish_receipt(const char *policy) {
    const char *path = getenv("LINGXI_ENFORCEMENT_RECEIPT_PATH");
    if (path == NULL || path[0] == '\0') {
        errno = EINVAL;
        return -1;
    }
    int fd = open(path, O_WRONLY | O_TRUNC | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return -1;
    size_t length = strlen(policy);
    ssize_t written = write(fd, policy, length);
    int saved_errno = errno;
    if (written == (ssize_t)length && fsync(fd) != 0) {
        saved_errno = errno;
        written = -1;
    }
    close(fd);
    errno = saved_errno;
    return written == (ssize_t)length ? 0 : -1;
}

static void close_inherited_descriptors(void) {
    long maximum = sysconf(_SC_OPEN_MAX);
    if (maximum < 0 || maximum > 65536)
        maximum = 65536;
    for (int fd = 3; fd < maximum; fd++)
        close(fd);
}

int main(int argc, char **argv) {
    if (argc < 3 || (strcmp(argv[1], "disabled") != 0 &&
                     strcmp(argv[1], "loopback_only") != 0)) {
        fprintf(stderr, "network_policy_unavailable: unsupported launcher policy\n");
        return 125;
    }
    if (strcmp(argv[1], "disabled") == 0) {
        if (install_disabled_network_policy() != 0) {
            fprintf(stderr, "network_policy_unavailable: seccomp install failed: %s\n",
                    strerror(errno));
            return 125;
        }
        if (publish_receipt("disabled\n") != 0) {
            fprintf(stderr, "network_policy_unavailable: receipt publish failed: %s\n",
                    strerror(errno));
            return 125;
        }
        unsetenv("LINGXI_ENFORCEMENT_RECEIPT_PATH");
    }
    // Disabled is enforced here with seccomp. LoopbackOnly is enforced by the
    // sockaddr-aware PRoot extension, which publishes its own receipt after
    // initialization. In both cases, closing inherited non-stdio descriptors
    // prevents smuggling a pre-connected Internet socket into the tracee tree.
    close_inherited_descriptors();
    execv(argv[2], &argv[2]);
    fprintf(stderr, "network_policy_unavailable: PRoot exec failed: %s\n",
            strerror(errno));
    return 126;
}

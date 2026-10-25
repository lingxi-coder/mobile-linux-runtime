#ifndef LX_MOUNT_PATH_GUARD_H
#define LX_MOUNT_PATH_GUARD_H

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Routing and write policy must select the same most-specific overlay. NULL
 * entries represent inactive mounts; siblings must match at a path boundary. */
static int lx_mount_longest_prefix(const char *path, const char *const prefixes[], size_t count) {
    int best = -1;
    size_t longest = 0;
    for (size_t index = 0; index < count; index++) {
        if (prefixes[index] == NULL) continue;
        size_t length = strlen(prefixes[index]);
        if (length > longest && !strncmp(path, prefixes[index], length) &&
            (path[length] == '/' || path[length] == '\0')) {
            best = (int)index;
            longest = length;
        }
    }
    return best;
}

/* Walk only physical rootfs directories. A guest HOME/overlay symlink must
 * never redirect mount creation or retirement into persistent user storage. */
static int lx_mount_parent_fd(int root_fd, const char *relative, char leaf[PATH_MAX]) {
    if (relative[0] == '/' || strlen(relative) >= PATH_MAX) {
        errno = EINVAL;
        return -1;
    }
    char path[PATH_MAX];
    snprintf(path, sizeof(path), "%s", relative);
    char *save = NULL;
    char *part = strtok_r(path, "/", &save);
    int parent = dup(root_fd);
    if (parent < 0) return -1;
    while (part != NULL) {
        if (!strcmp(part, ".") || !strcmp(part, "..")) {
            close(parent);
            errno = EINVAL;
            return -1;
        }
        char *next = strtok_r(NULL, "/", &save);
        if (next == NULL) {
            snprintf(leaf, PATH_MAX, "%s", part);
            return parent;
        }
        int child = openat(parent, part, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        int failure = errno;
        close(parent);
        if (child < 0) {
            errno = failure;
            return -1;
        }
        parent = child;
        part = next;
    }
    close(parent);
    errno = EINVAL;
    return -1;
}

/* The exact link bytes were captured at creation. Missing backing targets and
 * /var aliases cannot weaken ownership into a generic "is a symlink" check. */
static bool lx_mount_link_matches(int parent_fd, const char *leaf, const char *owned_target) {
    struct stat state;
    if (fstatat(parent_fd, leaf, &state, AT_SYMLINK_NOFOLLOW) < 0 || !S_ISLNK(state.st_mode))
        return false;
    char target[PATH_MAX];
    ssize_t count = readlinkat(parent_fd, leaf, target, sizeof(target) - 1);
    if (count < 0 || (size_t)count >= sizeof(target) - 1) return false;
    target[count] = '\0';
    return !strcmp(target, owned_target);
}

#endif

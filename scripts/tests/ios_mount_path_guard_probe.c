#define _POSIX_C_SOURCE 200809L
#define _DARWIN_C_SOURCE 1
#include "lx_mount_path_guard.h"
#include <assert.h>

int main(int argc, char **argv) {
    assert(argc == 2);
    const char *prefixes[] = {"/demo", "/demo/branch", "/demo/branch/item", NULL};
    assert(lx_mount_longest_prefix("/demo/branch/item/source", prefixes, 4) == 2);
    assert(lx_mount_longest_prefix("/demo/branch/sibling", prefixes, 4) == 1);
    assert(lx_mount_longest_prefix("/demo/branches", prefixes, 4) == 0);
    assert(lx_mount_longest_prefix("/demo2/branch", prefixes, 4) == -1);
    prefixes[2] = NULL;
    assert(lx_mount_longest_prefix("/demo/branch/item", prefixes, 4) == 1);
    int root = open(argv[1], O_RDONLY | O_DIRECTORY);
    assert(root >= 0);
    assert(mkdirat(root, "rootfs", 0700) == 0);
    assert(mkdirat(root, "host-home", 0700) == 0);
    int data = openat(root, "rootfs", O_RDONLY | O_DIRECTORY);
    int home = openat(root, "host-home", O_RDONLY | O_DIRECTORY);
    assert(data >= 0 && home >= 0);
    assert(mkdirat(data, "home", 0700) == 0);
    assert(mkdirat(data, "scaffold", 0700) == 0);
    assert(mkdirat(home, "projects", 0700) == 0);
    int project = openat(home, "projects", O_RDONLY | O_DIRECTORY);
    int sentinel = openat(project, "keep-source", O_CREAT | O_EXCL | O_WRONLY, 0600);
    assert(sentinel >= 0 && write(sentinel, "preserved", 9) == 9);
    close(sentinel);
    int guest_home = openat(data, "home", O_RDONLY | O_DIRECTORY);
    assert(symlinkat("../../host-home", guest_home, "lingxi") == 0);
    close(guest_home);

    char leaf[PATH_MAX];
    assert(lx_mount_parent_fd(data, "home/lingxi/projects", leaf) < 0);
    assert(lx_mount_parent_fd(data, "scaffold/../host-home", leaf) < 0);
    assert(lx_mount_parent_fd(data, "/absolute", leaf) < 0);
    assert(faccessat(project, "keep-source", F_OK, 0) == 0);

    int physical = lx_mount_parent_fd(data, "scaffold/mount", leaf);
    assert(physical >= 0 && !strcmp(leaf, "mount"));
    assert(symlinkat("../../host-home", physical, leaf) == 0);
    assert(lx_mount_link_matches(physical, leaf, "../../host-home"));
    assert(!lx_mount_link_matches(physical, leaf, "../../another-home"));
    assert(unlinkat(physical, leaf, 0) == 0);
    assert(symlinkat("../../another-home", physical, leaf) == 0);
    assert(!lx_mount_link_matches(physical, leaf, "../../host-home"));
    assert(unlinkat(physical, leaf, 0) == 0);
    assert(mkdirat(physical, leaf, 0700) == 0);
    assert(!lx_mount_link_matches(physical, leaf, "../../host-home"));
    assert(faccessat(project, "keep-source", F_OK, 0) == 0);
    close(physical); close(project); close(home); close(data); close(root);
    return 0;
}

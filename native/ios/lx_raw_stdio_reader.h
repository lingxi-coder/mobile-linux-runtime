#ifndef LX_RAW_STDIO_READER_H
#define LX_RAW_STDIO_READER_H

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stddef.h>
#include <sys/types.h>
#include <unistd.h>

enum lx_raw_read_state {
    LX_RAW_READ_WAIT,
    LX_RAW_READ_DATA,
    LX_RAW_READ_EOF,
    LX_RAW_READ_ERROR,
};

/* Must succeed before starting the guest: writes run under the close lock. */
static inline int lx_raw_prepare_writer(int fd) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0) return -1;
#ifdef F_SETNOSIGPIPE
    if (fcntl(fd, F_SETNOSIGPIPE, 1) < 0) return -1;
#else
    errno = ENOTSUP;
    return -1;
#endif
    return 0;
}

/* A completed poll is not EOF: only a zero-byte read proves it. */
static inline enum lx_raw_read_state lx_raw_read_step(
    int fd, void *buffer, size_t capacity, ssize_t *count, int *read_error
) {
    struct pollfd descriptor = { .fd = fd, .events = POLLIN };
    int ready = poll(&descriptor, 1, 250);
    if (ready < 0) {
        if (errno == EINTR) return LX_RAW_READ_WAIT;
        *read_error = errno;
        return LX_RAW_READ_ERROR;
    }
    if (ready == 0) return LX_RAW_READ_WAIT;
    *count = read(fd, buffer, capacity);
    if (*count > 0) return LX_RAW_READ_DATA;
    if (*count == 0) return LX_RAW_READ_EOF;
    if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) return LX_RAW_READ_WAIT;
    *read_error = errno;
    return LX_RAW_READ_ERROR;
}

#endif

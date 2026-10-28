#!/usr/bin/env python3
"""Observe the /dev/tty session reference with a live session leader.

Run on macOS with permission to open the new PTY's /dev/tty. Only processes
forked by this script are controlled; an error is a failed observation.
"""

import collections
import fcntl
import os
import pty
import select
import signal
import sys
import termios
import traceback


def probe():
    master, slave = pty.openpty()
    release_r, release_w = os.pipe()
    report_r, report_w = os.pipe()
    owner = os.fork()
    if owner == 0:
        try:
            os.close(master)
            os.close(release_w)
            os.close(report_r)
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            control = os.open("/dev/tty", os.O_WRONLY)
            os.close(control)
            writer = os.fork()
            if writer == 0:
                os.close(release_r)
                os.close(report_w)
                os.write(slave, b"__TAIL__")
                os.close(slave)
                os._exit(0)
            os.close(slave)
            _, status = os.waitpid(writer, 0)
            if status:
                os._exit(95)
            os.write(report_w, b"W")
            os.close(report_w)
            if os.read(release_r, 1) != b"G":
                os._exit(96)
            os.close(release_r)
            os._exit(0)
        except BaseException:
            traceback.print_exc()
            os._exit(97)
    os.close(slave)
    os.close(release_r)
    os.close(report_w)
    try:
        if not select.select([report_r], [], [], 5)[0] or os.read(report_r, 1) != b"W":
            raise RuntimeError("guardian setup failed")
        os.set_blocking(master, False)
        assert os.read(master, 4096) == b"__TAIL__"
        try:
            raise AssertionError(f"unexpected early read: {os.read(master, 4096)!r}")
        except BlockingIOError:
            pass
        os.write(release_w, b"G")
        os.close(release_w)
        release_w = -1
        _, status = os.waitpid(owner, 0)
        owner = -1
        assert status == 0, status
        assert os.read(master, 4096) == b""
    finally:
        os.close(report_r)
        os.close(master)
        if release_w != -1:
            os.close(release_w)
        if owner != -1:
            # This is the script's exclusively owned, unreaped direct child.
            os.kill(owner, signal.SIGKILL)
            os.waitpid(owner, 0)


if __name__ == "__main__":
    if sys.platform != "darwin":
        raise SystemExit("native macOS is required")
    counts = collections.Counter()
    for _ in range(20):
        probe()
        counts["tail preserved; EOF only after guardian exits"] += 1
    print(dict(counts))

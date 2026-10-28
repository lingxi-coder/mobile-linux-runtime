#!/usr/bin/env python3
"""Observe macOS PTY tail/EOF with and without a retained slave descriptor."""

import collections
import errno
import os
import pty
import sys


def probe(keep):
    marker = b"pty-eof-tail!"
    master, slave = pty.openpty()
    gate_read, gate_write = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(master)
        os.close(gate_write)
        try:
            if os.read(gate_read, 1) != b"G":
                os._exit(90)
            os.close(gate_read)
            if os.write(slave, marker) != len(marker):
                os._exit(91)
            os.close(slave)
            os._exit(0)
        except BaseException:
            os._exit(92)
    os.close(gate_read)
    if not keep:
        os.close(slave)
    os.write(gate_write, b"G")
    os.close(gate_write)
    _, status = os.waitpid(pid, 0)
    if not os.WIFEXITED(status) or os.WEXITSTATUS(status):
        raise AssertionError(f"writer failed: {status}")
    os.set_blocking(master, False)
    data = b""
    end = "open"
    while True:
        try:
            part = os.read(master, 4096)
            if not part:
                end = "eof"
                break
            data += part
        except BlockingIOError:
            break
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            end = "eio"
            break
    result = (data == marker, end)
    if keep:
        assert result == (True, "open"), result
        os.close(slave)
        try:
            assert os.read(master, 4096) == b""
        except OSError as error:
            if error.errno != errno.EIO:
                raise
    os.close(master)
    return result


def main():
    if sys.platform != "darwin":
        raise SystemExit("this observation requires a native macOS runner")
    for keep in (False, True):
        counts = collections.Counter(probe(keep) for _ in range(40))
        print(f"keeper={keep}: {dict(counts)}", flush=True)
    print("The delayed reader is intentional; this is a scheduling counterexample.")


if __name__ == "__main__":
    main()

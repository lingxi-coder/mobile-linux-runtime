#!/usr/bin/env python3
"""Test exec observations of only the direct children created in this script."""

import ctypes
import errno
import os
import pty
import select
import signal
import struct
import sys
import tempfile
import time
from pathlib import Path

if not sys.platform.startswith("linux"):
    raise SystemExit("native Linux is required")

libc = ctypes.CDLL(None, use_errno=True)
libc.ptrace.argtypes = [ctypes.c_uint, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
libc.ptrace.restype = ctypes.c_long
libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
libc.prctl.restype = ctypes.c_int

PTRACE_TRACEME = 0
PTRACE_CONT = 7
PTRACE_DETACH = 17
PTRACE_SETOPTIONS = 0x4200
PTRACE_O_TRACEEXEC = 0x10
PTRACE_O_EXITKILL = 0x100000
PTRACE_EVENT_EXEC = 4
PR_SET_PDEATHSIG = 1


def ptrace(request, pid=0, data=0):
    ctypes.set_errno(0)
    result = libc.ptrace(request, pid, None, ctypes.c_void_p(data))
    if result == -1:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def parent_death_guard():
    parent = os.getppid()
    if libc.prctl(PR_SET_PDEATHSIG, signal.SIGKILL, 0, 0, 0) != 0:
        os._exit(119)
    if os.getppid() != parent:
        os._exit(119)


class OwnedChild:
    def __init__(self, pid):
        self.pid = pid
        self.reaped = False

    def wait(self, flags=0):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pid, status = os.waitpid(self.pid, flags | os.WNOHANG)
            if pid:
                if os.WIFEXITED(status) or os.WIFSIGNALED(status):
                    self.reaped = True
                return status
            time.sleep(0.005)
        raise TimeoutError(f"child {self.pid} did not change state")

    def cleanup(self):
        if not self.reaped:
            try:
                # Exclusively owned, direct, unreaped child, never a scanned PID.
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(self.pid, 0)
            except ChildProcessError:
                pass
            self.reaped = True


def expected_exit(status, code):
    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == code, status


def fast_elf_zombie_is_not_visible_as_target_exe():
    pid = os.fork()
    if pid == 0:
        parent_death_guard()
        os.execv("/bin/true", ["/bin/true"])
    child = OwnedChild(pid)
    try:
        deadline = time.monotonic() + 5
        while True:
            info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
            if info is not None:
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("short-lived ELF did not exit")
            time.sleep(0.005)
        assert info.si_code == os.CLD_EXITED and info.si_status == 0, info
        try:
            os.stat(f"/proc/{pid}/exe")
        except OSError as error:
            assert error.errno in (errno.ENOENT, errno.ESRCH), error
        else:
            raise AssertionError("expected proc exe to disappear for zombie")
        expected_exit(child.wait(), 0)
    finally:
        child.cleanup()


def linux_pty_last_close_preserves_tail():
    master, slave = pty.openpty()
    gate_read, gate_write = os.pipe()
    pid = os.fork()
    if pid == 0:
        parent_death_guard()
        os.close(master)
        os.close(gate_write)
        if os.read(gate_read, 1) != b"G":
            os._exit(90)
        os.close(gate_read)
        os.write(slave, b"__LINUX_TAIL__")
        os.close(slave)
        os._exit(0)
    child = OwnedChild(pid)
    os.close(gate_read)
    os.close(slave)
    try:
        os.write(gate_write, b"G")
        os.close(gate_write)
        gate_write = -1
        expected_exit(child.wait(), 0)
        assert os.read(master, 4096) == b"__LINUX_TAIL__"
        poller = select.poll()
        poller.register(master, select.POLLIN | select.POLLHUP)
        events = poller.poll(1000)
        assert events and events[0][1] & select.POLLHUP, events
        try:
            os.read(master, 4096)
        except OSError as error:
            assert error.errno == errno.EIO, error
        else:
            raise AssertionError("expected Linux master EIO after slave hangup")
    finally:
        if gate_write != -1:
            os.close(gate_write)
        os.close(master)
        child.cleanup()


def live_shebang_has_interpreter_identity(script):
    gate_read, gate_write = os.pipe()
    ready_read, ready_write = os.pipe()
    pid = os.fork()
    if pid == 0:
        parent_death_guard()
        os.close(gate_write)
        os.close(ready_read)
        os.dup2(gate_read, 0)
        os.dup2(ready_write, 1)
        os.close(gate_read)
        os.close(ready_write)
        os.execv(str(script), [str(script)])
    child = OwnedChild(pid)
    os.close(gate_read)
    os.close(ready_write)
    try:
        assert select.select([ready_read], [], [], 5)[0], "script did not start"
        assert os.read(ready_read, 6) == b"ready\n"
        target = os.stat(script)
        executable = os.stat(f"/proc/{pid}/exe")
        assert (target.st_dev, target.st_ino) != (executable.st_dev, executable.st_ino)
        os.write(gate_write, b"go\n")
        expected_exit(child.wait(), 23)
    finally:
        os.close(ready_read)
        os.close(gate_write)
        child.cleanup()


def trace_one(executable, expected_code, kill_before_exec=False):
    status_read, status_write = os.pipe2(os.O_CLOEXEC)
    pid = os.fork()
    if pid == 0:
        os.close(status_read)
        parent_death_guard()
        try:
            ptrace(PTRACE_TRACEME)
            os.kill(os.getpid(), signal.SIGSTOP)
            os.execv(str(executable), [str(executable)])
        except OSError as error:
            os.write(status_write, struct.pack("i", error.errno))
            os._exit(127)
    child = OwnedChild(pid)
    os.close(status_write)
    try:
        status = child.wait(os.WUNTRACED)
        assert os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGSTOP, status
        if kill_before_exec:
            os.kill(pid, signal.SIGKILL)
            status = child.wait()
            assert os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGKILL
            assert os.read(status_read, 4) == b"", "killed process must close the pipe"
            return
        ptrace(PTRACE_SETOPTIONS, pid, PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL)
        ptrace(PTRACE_CONT, pid)
        status = child.wait()
        if expected_code is None:
            expected_exit(status, 127)
            failure = os.read(status_read, 4)
            assert len(failure) == 4 and struct.unpack("i", failure)[0] == errno.ENOENT
            return
        assert os.WIFSTOPPED(status), status
        assert os.WSTOPSIG(status) == signal.SIGTRAP, status
        assert status >> 16 == PTRACE_EVENT_EXEC, status
        # The kernel exec event is the proof. Pipe EOF alone also occurs in
        # the killed-before-exec case above, so it is only a consistency check.
        assert os.read(status_read, 4) == b""
        ptrace(PTRACE_DETACH, pid)
        expected_exit(child.wait(), expected_code)
    finally:
        os.close(status_read)
        child.cleanup()


def main():
    with tempfile.TemporaryDirectory(prefix="pty-exec-probe-") as directory:
        script = Path(directory) / "shell-script"
        script.write_text("#!/bin/sh\nprintf 'ready\\n'\nIFS= read -r gate\nexit 23\n")
        script.chmod(0o700)
        for _ in range(20):
            fast_elf_zombie_is_not_visible_as_target_exe()
        print("PASS: 20 short-lived ELF zombies invisible to /proc/PID/exe", flush=True)
        for _ in range(20):
            linux_pty_last_close_preserves_tail()
        print("PASS: 20 Linux last-slave-close tails followed by POLLHUP/EIO", flush=True)
        live_shebang_has_interpreter_identity(script)
        print("PASS: live shebang /proc/PID/exe differs from script inode", flush=True)
        # This variant does not wait on stdin after exec.
        script.write_text("#!/bin/sh\nexit 23\n")
        for _ in range(20):
            trace_one("/bin/true", 0)
            trace_one(script, 23)
        print("PASS: 20 ELF and 20 shebang PTRACE_EVENT_EXEC observations", flush=True)
        trace_one("/definitely-missing-pty-probe", None)
        print("PASS: missing exec reports ENOENT without an exec event", flush=True)
        trace_one("/bin/true", 0, kill_before_exec=True)
        print("PASS: killed-before-exec closes the pipe without an exec event", flush=True)


if __name__ == "__main__":
    main()

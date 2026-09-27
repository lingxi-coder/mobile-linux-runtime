/*
 * Copyright (C) 2025 OpenMinis contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License, version 3.
 */
package com.openminis.app.sandbox

/**
 * JNI declarations for the OpenMinis PTY bridge bundled by the Android build.
 *
 * The package is intentionally kept identical to OpenMinis because the pinned
 * native library exports JNI symbols with this fully-qualified class name.
 */
object PtyBridge {
    init {
        System.loadLibrary("pty_bridge")
    }

    @JvmStatic
    external fun forkExec(
        cmd: String,
        argv: Array<String>,
        envp: Array<String>,
        cwd: String?,
        cols: Int,
        rows: Int,
        outPid: IntArray,
    ): Int

    @JvmStatic
    external fun readBytes(fd: Int, buf: ByteArray, off: Int, len: Int): Int

    @JvmStatic
    external fun writeBytes(fd: Int, buf: ByteArray, off: Int, len: Int): Int

    @JvmStatic
    external fun setWindowSize(fd: Int, cols: Int, rows: Int): Int

    @JvmStatic
    external fun closeFd(fd: Int): Int

    @JvmStatic
    external fun sendSignal(pid: Int, signal: Int): Int

    @JvmStatic
    external fun waitFor(pid: Int): Int
}

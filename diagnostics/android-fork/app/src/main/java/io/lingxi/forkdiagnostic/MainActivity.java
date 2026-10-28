package io.lingxi.forkdiagnostic;

import android.app.Activity;
import android.os.Bundle;
import android.os.Process;
import android.util.Log;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.TimeUnit;

public class MainActivity extends Activity {
    private static final String TAG = "LxForkProbe";
    private File root;
    private File nativeDir;
    private File temp;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        new Thread(() -> {
            try {
                root = new File(getFilesDir(), "root");
                nativeDir = new File(getApplicationInfo().nativeLibraryDir);
                temp = new File(getFilesDir(), "temp");
                if (!temp.mkdirs() && !temp.isDirectory()) throw new Exception("temp mkdir");
                copyAsset("bin/busybox");
                copyAsset("lib/ld-musl-x86_64.so.1");
                for (String operation : Arrays.asList("fork", "clone", "clone-untraced", "clone3")) {
                    probe("direct-" + operation, null, false, false, operation);
                    probe("current-" + operation, "libproot_current.so", false, false, operation);
                }
                probe("baseline-fork", "libproot_baseline.so", false, false, "fork");
                probe("current-fork-no-proot-seccomp", "libproot_current.so", true, false, "fork");
                shell("current-rootfs", "libproot_current.so", false, false);
                shell("current-rootfs-no-proot-seccomp", "libproot_current.so", true, false);
                shell("current-rootfs-network-disabled", "libproot_current.so", false, true);
                shell("old-rootfs", "libproot_old.so", false, false);
                shell("baseline-rootfs", "libproot_baseline.so", false, false);
                ownerProbe();
                Log.i(TAG, "DIAGNOSTIC_END");
            } catch (Throwable failure) {
                Log.e(TAG, "DIAGNOSTIC_ERROR " + failure, failure);
            } finally {
                finish();
            }
        }).start();
    }

    private void copyAsset(String path) throws Exception {
        File output = new File(root, path);
        if (!output.getParentFile().mkdirs() && !output.getParentFile().isDirectory()) throw new Exception("asset mkdir");
        try (InputStream input = getAssets().open(path); FileOutputStream stream = new FileOutputStream(output)) {
            byte[] buffer = new byte[16384];
            for (int n; (n = input.read(buffer)) != -1;) stream.write(buffer, 0, n);
        }
        if (!output.setExecutable(true, true)) throw new Exception("asset chmod");
    }

    private List<String> base(String proot, boolean policy, File guestRoot) {
        List<String> cmd = new ArrayList<>();
        if (policy) {
            cmd.add(new File(nativeDir, "libmobile_linux_policy_launcher.so").getAbsolutePath());
            cmd.add("disabled");
        }
        cmd.add(new File(nativeDir, proot).getAbsolutePath());
        cmd.add("-r"); cmd.add(guestRoot.getAbsolutePath());
        cmd.add("-b"); cmd.add("/dev");
        cmd.add("-b"); cmd.add("/proc");
        cmd.add("-w"); cmd.add("/");
        return cmd;
    }

    private void probe(String name, String proot, boolean noSeccomp, boolean policy, String operation) throws Exception {
        List<String> cmd = proot == null ? new ArrayList<>() : base(proot, policy, new File("/"));
        cmd.add(new File(nativeDir, "libforkprobe.so").getAbsolutePath());
        cmd.add(operation);
        run(name, cmd, noSeccomp, policy);
    }

    private void ownerProbe() throws Exception {
        List<String> cmd = new ArrayList<>();
        cmd.add(new File(nativeDir, "libownerprobe.so").getAbsolutePath());
        cmd.add(new File(nativeDir, "libproot_current.so").getAbsolutePath());
        cmd.add(nativeDir.getAbsolutePath());
        cmd.add(temp.getAbsolutePath());
        run("current-owner", cmd, false, false);
    }

    private void shell(String name, String proot, boolean noSeccomp, boolean policy) throws Exception {
        List<String> cmd = base(proot, policy, root);
        cmd.add("/bin/busybox"); cmd.add("sh"); cmd.add("-c");
        cmd.add("set -e; /bin/busybox echo FORK_FIRST; /bin/busybox echo FORK_SECOND");
        run(name, cmd, noSeccomp, policy);
    }

    private void run(String name, List<String> command, boolean noSeccomp, boolean policy) throws Exception {
        ProcessBuilder builder = new ProcessBuilder(command).redirectErrorStream(true);
        builder.environment().put("LINGXI_RUNTIME_OWNER_PID", Integer.toString(Process.myPid()));
        String loader = command.toString().contains("libproot_old.so") ? "libproot-loader-old.so"
                : command.toString().contains("libproot_baseline.so") ? "libproot-loader-baseline.so"
                : "libproot-loader.so";
        builder.environment().put("PROOT_LOADER", new File(nativeDir, loader).getAbsolutePath());
        builder.environment().put("PROOT_TMP_DIR", temp.getAbsolutePath());
        if (noSeccomp) builder.environment().put("PROOT_NO_SECCOMP", "1");
        else builder.environment().remove("PROOT_NO_SECCOMP");
        if (policy) {
            File receipt = new File(temp, "policy-receipt-" + name);
            if (!receipt.createNewFile()) throw new Exception("receipt exists");
            builder.environment().put("LINGXI_ENFORCEMENT_RECEIPT_PATH", receipt.getAbsolutePath());
        }
        java.lang.Process process = builder.start();
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        Thread reader = new Thread(() -> {
            try (InputStream input = process.getInputStream()) {
                byte[] buffer = new byte[4096];
                for (int n; (n = input.read(buffer)) != -1;) {
                    synchronized (output) {
                        int remaining = 16000 - output.size();
                        if (remaining > 0) output.write(buffer, 0, Math.min(n, remaining));
                    }
                }
            } catch (Exception failure) {
                Log.w(TAG, "reader " + name + " " + failure);
            }
        });
        reader.setDaemon(true);
        reader.start();
        boolean ended = process.waitFor(name.equals("current-owner") ? 60 : 15, TimeUnit.SECONDS);
        if (!ended) { process.destroyForcibly(); process.waitFor(); }
        reader.join(2000);
        boolean heldPipe = reader.isAlive();
        String text;
        synchronized (output) { text = new String(output.toByteArray(), StandardCharsets.UTF_8); }
        text = text.replace('\n', '|').replace('\r', ' ');
        Log.i(TAG, "RESULT " + name + " exit=" + (ended ? Integer.toString(process.exitValue()) : "timeout") + " heldPipe=" + heldPipe + " output=" + text);
    }
}

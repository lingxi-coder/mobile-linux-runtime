package io.lingxi.mobilelinux.gradle;

import com.android.build.api.dsl.ApplicationExtension;
import groovy.json.JsonSlurper;
import java.security.MessageDigest;
import java.io.*;
import java.nio.file.*;
import java.util.*;
import java.util.zip.*;
import org.gradle.api.*;

/** Packaging is checked on the final APK, including transitive native support. */
public final class MobileLinuxPlugin implements Plugin<Project> {
    private static final List<String> HELPERS = List.of("libproot.so", "libproot-loader.so", "libmobile_linux_policy_launcher.so", "libpty_bridge.so", "libmksh.so", "libtoybox.so");
    public void apply(Project project) {
        project.getPlugins().withId("com.android.application", ignored -> {
            ApplicationExtension android = project.getExtensions().getByType(ApplicationExtension.class);
            android.getPackaging().getJniLibs().setUseLegacyPackaging(true);
            for (String helper : HELPERS) android.getPackaging().getJniLibs().getKeepDebugSymbols().add("**/" + helper);
            project.afterEvaluate(p -> {
                if (!Boolean.TRUE.equals(android.getPackaging().getJniLibs().getUseLegacyPackaging()))
                    throw new GradleException("Mobile Linux helpers require useLegacyPackaging=true");
            });
            var verify = project.getTasks().register("verifyMobileLinuxApks", task -> {
                task.setGroup("verification");
                task.doLast(unused -> verifyApks(project));
            });
            project.getTasks().matching(t -> t.getName().startsWith("assemble") && !t.getName().endsWith("AndroidTest") && !t.getName().endsWith("UnitTest")).configureEach(t -> t.finalizedBy(verify));
        });
    }
    private static void verifyApks(Project project) {
        String mode = String.valueOf(project.findProperty("mobileLinuxPackaging") == null ? "full" : project.findProperty("mobileLinuxPackaging"));
        if (!mode.equals("full") && !mode.equals("native-support")) throw new GradleException("mobileLinuxPackaging must be full or native-support");
        File directory = project.getLayout().getBuildDirectory().dir("outputs/apk").get().getAsFile();
        try (var paths = Files.walk(directory.toPath())) {
            var apks = paths.filter(p -> p.toString().endsWith(".apk"))
                .filter(p -> !directory.toPath().relativize(p).startsWith("androidTest"))
                .toList();
            if (apks.isEmpty()) throw new GradleException("No final APK found for Mobile Linux verification");
            for (Path apk : apks) try (ZipFile zip = new ZipFile(apk.toFile())) {
                ZipEntry metadata = zip.getEntry("assets/mobile-linux/native-manifest.json");
                if (metadata == null) throw new GradleException("Missing native helper provenance in " + apk);
                Map<?, ?> manifest;
                try (InputStream input = zip.getInputStream(metadata)) { manifest = (Map<?, ?>) new JsonSlurper().parse(input); }
                Map<String, String> hashes = new HashMap<>();
                for (Object value : (List<?>) manifest.get("artifacts")) {
                    Map<?, ?> artifact = (Map<?, ?>) value;
                    hashes.put((String) artifact.get("path"), (String) artifact.get("sha256"));
                }
                for (String abi : List.of("arm64-v8a", "x86_64")) {
                    var required = new ArrayList<>(HELPERS);
                    if (mode.equals("full")) required.add("libmobile_linux_runtime.so");
                    for (String name : required) {
                        ZipEntry entry = zip.getEntry("lib/" + abi + "/" + name);
                        if (entry == null) throw new GradleException(apk + " missing " + abi + "/" + name);
                        if (HELPERS.contains(name)) {
                            String expected = hashes.get("jniLibs/" + abi + "/" + name);
                            if (expected == null) throw new GradleException("No pinned helper hash for " + name);
                            try (InputStream input = zip.getInputStream(entry)) {
                                MessageDigest digest;
                                try { digest = MessageDigest.getInstance("SHA-256"); } catch (java.security.NoSuchAlgorithmException e) { throw new GradleException("SHA-256 unavailable", e); }
                                byte[] buffer = new byte[65536]; int count;
                                while ((count = input.read(buffer)) >= 0) digest.update(buffer, 0, count);
                                if (!HexFormat.of().formatHex(digest.digest()).equals(expected)) throw new GradleException("Final APK helper differs from native artifact: " + name);
                            }
                        }
                        try (InputStream input = zip.getInputStream(entry)) {
                            byte[] header = input.readNBytes(20);
                            int machine = abi.equals("arm64-v8a") ? 183 : 62;
                            if (header.length != 20 || header[0] != 127 || header[1] != 69 || header[2] != 76 || header[3] != 70 || header[4] != 2 || header[5] != 1 || (header[18] & 255) + ((header[19] & 255) << 8) != machine)
                                throw new GradleException(apk + " invalid ELF64 ABI for " + name);
                        }
                    }
                    if (mode.equals("native-support") && zip.getEntry("lib/" + abi + "/libmobile_linux_runtime.so") != null)
                        throw new GradleException("native-support must not package a second Rust FFI runtime");
                    if (mode.equals("full") && zip.getEntry("lib/" + abi + "/libandroid_aar.so") != null)
                        throw new GradleException("Use native-support when embedding LingXi's Rust core");
                }
            }
        } catch (IOException failure) { throw new GradleException("Cannot verify Mobile Linux APK", failure); }
    }
}

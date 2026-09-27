#!/usr/bin/env python3
"""Build a Maven-only sample and exercise a verified real rootfs on an Android device."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

PACKAGE = "io.lingxi.mobilelinux.sample"
EXAMPLE = Path(__file__).resolve().parent
SDK = EXAMPLE.parents[1]

def run(argv, **kwargs):
    print("+ " + " ".join(map(str, argv)), flush=True)
    return subprocess.run(list(map(str, argv)), check=True, **kwargs)

def build_sample(project: Path, report: Path, maven_repo: Path, variant: str) -> None:
    with (report / "build.log").open("w") as log:
        task_variant = variant[0].upper() + variant[1:]
        run([SDK / "android/gradlew", "-p", project, "--project-cache-dir", report / "gradle-cache",
             f"-Pkotlin.project.persistent.dir={report / 'kotlin'}",
             f"-PsdkMavenRepo={maven_repo.resolve()}",
             f"-PsdkTestBuildType={variant}",
             f"-PsdkAcceptanceTest={str(variant == 'release').lower()}", "--refresh-dependencies",
             f":app:assemble{task_variant}",
             f":app:assemble{task_variant}AndroidTest", "--console=plain"],
            cwd=project, stdout=log, stderr=subprocess.STDOUT)
    if variant == "release" and "> Task :app:minifyReleaseWithR8" not in (report / "build.log").read_text():
        raise SystemExit("release sample did not run its R8 minification gate")


def verify_device(adb: list[str], report: Path, variant: str, actual_sha: str) -> None:
    result = subprocess.run([*adb, "shell", "am", "instrument", "-w", "-r", "-e", "class",
        "io.lingxi.mobilelinux.sample.RealRuntimeSmokeTest",
        PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=960)
    (report / "instrumentation.log").write_text(result.stdout)
    # Vendor log buffers can contain non-UTF-8 bytes; keep the acceptance
    # result independent of unrelated binary diagnostic payloads.
    logcat = subprocess.check_output([*adb, "logcat", "-d", "-b", "all"]).decode("utf-8", errors="replace")
    (report / "device-logcat.log").write_text(logcat)
    if result.returncode or "OK (1 test)" not in result.stdout:
        print(result.stdout)
        raise SystemExit("actual device smoke failed; see instrumentation.log and device-logcat.log")
    if variant == "debug":
        evidence = subprocess.check_output([*adb, "exec-out", "run-as", PACKAGE,
            "cat", "files/sdk-smoke-evidence.json"], text=True)
    else:
        marker = "SDK_SMOKE_EVIDENCE="
        reports = [line.split(marker, 1)[1] for line in logcat.splitlines() if marker in line]
        if not reports:
            raise SystemExit("minified release test did not emit acceptance evidence")
        evidence = reports[-1]
    parsed = json.loads(evidence)
    if parsed["rootfsSha256"] != actual_sha or len(parsed["checks"]) != 10:
        raise SystemExit("device acceptance evidence is incomplete")
    (report / "evidence.json").write_text(evidence)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adb", type=Path, required=True)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--maven-repo", type=Path, required=True)
    parser.add_argument("--rootfs-dir", type=Path, required=True)
    parser.add_argument("--report-dir", type=Path, required=True)
    parser.add_argument("--variant", choices=("debug", "release"), default="debug")
    args = parser.parse_args()
    rootfs = args.rootfs_dir.resolve()
    report = args.report_dir.resolve()
    if report == SDK or report.is_relative_to(SDK):
        raise SystemExit("report-dir must be outside the SDK source checkout")
    report.mkdir(parents=True, exist_ok=True)
    project = report / "project"
    if project.exists():
        shutil.rmtree(project)
    shutil.copytree(EXAMPLE, project, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("build", ".gradle", ".kotlin", "__pycache__"))
    manifest_path = rootfs / "evidence/rootfs-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    archive = rootfs / "rootfs.tar.gz"
    actual_sha = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    if manifest["archive"]["sha256"] != actual_sha or manifest["platform"] != "android":
        raise SystemExit("rootfs archive bytes or platform differ from verified Android manifest")
    inputs = [(archive, "rootfs.tar.gz"), (manifest_path, "rootfs-manifest.json"),
              (rootfs / "evidence/rootfs.spdx.json", "rootfs.spdx.json")]
    if args.variant == "release":
        # A release APK is not debuggable, so the test APK supplies the caller's bytes.
        assets = project / "app/src/androidTest/assets/sdk-smoke-input"
        assets.mkdir(parents=True, exist_ok=True)
        for source, name in inputs:
            # AAPT expands .gz assets and drops the suffix; use an opaque name
            # so the archive bytes stay identical to the verified manifest.
            asset_name = "rootfs-archive.bin" if name == "rootfs.tar.gz" else name
            shutil.copy2(source, assets / asset_name)
    build_sample(project, report, args.maven_repo, args.variant)
    adb = [str(args.adb), "-s", args.serial]
    for apk in (f"{args.variant}/app-{args.variant}.apk",
                f"androidTest/{args.variant}/app-{args.variant}-androidTest.apk"):
        run([*adb, "install", "-r", project / "app/build/outputs/apk" / apk])
    if args.variant == "debug":
        run([*adb, "shell", "run-as", PACKAGE, "mkdir", "-p", "files/sdk-smoke-input"])
        for source, name in inputs:
            with source.open("rb") as stream:
                run([*adb, "exec-in", "run-as", PACKAGE, "dd",
                     f"of=files/sdk-smoke-input/{name}", "bs=1048576"], stdin=stream)
    verify_device(adb, report, args.variant, actual_sha)
    print("Actual Android SDK smoke passed: " + str(report))

if __name__ == "__main__": main()

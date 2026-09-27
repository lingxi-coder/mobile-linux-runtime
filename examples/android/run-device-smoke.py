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

def main():
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
    shutil.copytree(EXAMPLE, project, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("build", ".gradle", ".kotlin", "__pycache__"))
    manifest_path = rootfs / "evidence/rootfs-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    archive = rootfs / "rootfs.tar.gz"
    actual_sha = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    if manifest["archive"]["sha256"] != actual_sha or manifest["platform"] != "android":
        raise SystemExit("rootfs archive bytes or platform differ from verified Android manifest")
    with (report / "build.log").open("w") as log:
        run([SDK / "android/gradlew", "-p", project, "--project-cache-dir", report / "gradle-cache",
             f"-Pkotlin.project.persistent.dir={report / 'kotlin'}",
             f"-PsdkMavenRepo={args.maven_repo.resolve()}",
             f"-PsdkTestBuildType={args.variant}", "--refresh-dependencies",
             f":app:assemble{args.variant.title()}",
             f":app:assemble{args.variant.title()}AndroidTest", "--console=plain"],
            cwd=project, stdout=log, stderr=subprocess.STDOUT)
    adb = [str(args.adb), "-s", args.serial]
    for apk in (f"{args.variant}/app-{args.variant}.apk",
                f"androidTest/{args.variant}/app-{args.variant}-androidTest.apk"):
        run([*adb, "install", "-r", project / "app/build/outputs/apk" / apk])
    run([*adb, "shell", "run-as", PACKAGE, "mkdir", "-p", "files/sdk-smoke-input"])
    for source, name in [(archive, "rootfs.tar.gz"), (manifest_path, "rootfs-manifest.json"),
                         (rootfs / "evidence/rootfs.spdx.json", "rootfs.spdx.json")]:
        with source.open("rb") as stream:
            run([*adb, "exec-in", "run-as", PACKAGE, "dd",
                 f"of=files/sdk-smoke-input/{name}", "bs=1048576"], stdin=stream)
    result = subprocess.run([*adb, "shell", "am", "instrument", "-w", "-r", "-e", "class",
        "io.lingxi.mobilelinux.sample.RealRuntimeSmokeTest",
        PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=960)
    (report / "instrumentation.log").write_text(result.stdout)
    logcat = subprocess.check_output([*adb, "logcat", "-d", "-b", "all"], text=True)
    (report / "device-logcat.log").write_text(logcat)
    if result.returncode or "OK (1 test)" not in result.stdout:
        print(result.stdout)
        raise SystemExit("actual device smoke failed; see instrumentation.log and device-logcat.log")
    evidence = subprocess.check_output([*adb, "exec-out", "run-as", PACKAGE,
        "cat", "files/sdk-smoke-evidence.json"], text=True)
    parsed = json.loads(evidence)
    if parsed["rootfsSha256"] != actual_sha or len(parsed["checks"]) != 10:
        raise SystemExit("device acceptance evidence is incomplete")
    (report / "evidence.json").write_text(evidence)
    print("Actual Android SDK smoke passed: " + str(report))

if __name__ == "__main__": main()

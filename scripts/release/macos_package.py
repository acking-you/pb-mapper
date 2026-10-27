#!/usr/bin/env python3
"""Sign, notarize, staple and verify a pb-mapper release (macOS only)."""

import argparse
import base64
import contextlib
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import secrets
import shlex
import shutil
import subprocess
import tempfile
import time


def run(*args):
    result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True)
    if result.returncode:
        # Never include command arguments: security/notarytool receive credentials.
        raise RuntimeError(f"{Path(args[0]).name} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout + result.stderr



def stapler(action, path):
    # Even validation contacts Apple's ticket service. Retry transient URL
    # transport errors, never a rejected signature or invalid/missing ticket.
    for delay in (5, 15, 45, None):
        try:
            return run("xcrun", "stapler", action, path)
        except RuntimeError as error:
            if delay is None or not re.search(
                    r"NSURLErrorDomain Code=-(?:1001|1003|1004|1005|1006|1009)\b", str(error)):
                raise
            print(f"Apple ticket service network failure; retrying {action} in {delay}s…", flush=True)
            time.sleep(delay)


def required(name):
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"Missing required environment variable: {name}")
    return value


@contextlib.contextmanager
def credentials(work):
    """CI uses a disposable keychain; local callers may use an existing profile."""
    if os.environ.get("MACOS_CERTIFICATE_P12_BASE64"):
        password = required("MACOS_CERTIFICATE_PASSWORD")
        p12 = work / "identity.p12"
        p12.write_bytes(base64.b64decode(required("MACOS_CERTIFICATE_P12_BASE64"), validate=True))
        p12.chmod(0o600)
        keychain = work / "signing.keychain-db"
        keychain_password = secrets.token_urlsafe(32)
        search_list = shlex.split(run("security", "list-keychains", "-d", "user"))
        run("security", "create-keychain", "-p", keychain_password, keychain)
        try:
            run("security", "set-keychain-settings", "-lut", "21600", keychain)
            run("security", "unlock-keychain", "-p", keychain_password, keychain)
            run("security", "import", p12, "-k", keychain, "-P", password,
                "-T", "/usr/bin/codesign")
            run("security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:",
                "-s", "-k", keychain_password, keychain)
            # --keychain limits private-key lookup, but certificate-chain lookup
            # still needs the intermediate in the user's keychain search list.
            run("security", "list-keychains", "-d", "user", "-s", *search_list, keychain)
            identities = run("security", "find-identity", "-v", "-p", "codesigning", keychain)
            if required("MACOS_SIGNING_IDENTITY") not in identities:
                raise RuntimeError("The configured Developer ID identity is not valid in the imported keychain")
            p12.unlink()
            key = work / "AuthKey.p8"
            key.write_text(required("APPLE_NOTARY_PRIVATE_KEY"))
            key.chmod(0o600)
            profile = "pb-mapper-release"
            run("xcrun", "notarytool", "store-credentials", profile,
                "--key", key, "--key-id", required("APPLE_NOTARY_KEY_ID"),
                "--issuer", required("APPLE_NOTARY_ISSUER_ID"), "--keychain", keychain)
            key.unlink()
            yield ["--keychain", str(keychain)], ["--keychain-profile", profile, "--keychain", str(keychain)]
        finally:
            try:
                run("security", "list-keychains", "-d", "user", "-s", *search_list)
            finally:
                run("security", "delete-keychain", keychain)
    else:
        profile = required("APPLE_NOTARY_PROFILE")
        keychain = os.environ.get("MACOS_SIGNING_KEYCHAIN")
        signing = ["--keychain", keychain] if keychain else []
        notary_keychain = os.environ.get("APPLE_NOTARY_KEYCHAIN")
        notary = ["--keychain-profile", profile]
        if notary_keychain:
            notary += ["--keychain", notary_keychain]
        yield signing, notary


def macho_files(app):
    # Do not follow framework symlinks and sign the same binary multiple times.
    magics = {bytes.fromhex(x) for x in ("cffaedfe", "feedfacf", "cefaedfe", "feedface",
                                        "cafebabe", "bebafeca", "cafebabf", "bfbafeca")}
    files = []
    for path in app.rglob("*"):
        if path.is_file() and not path.is_symlink():
            with path.open("rb") as handle:
                if handle.read(4) in magics:
                    files.append(path)
    return sorted(files)


def audit_bundle(app, arch):
    info_path = app / "Contents/Info.plist"
    info = plistlib.loads(info_path.read_bytes())
    executable = "Contents/MacOS/" + info["CFBundleExecutable"]
    for name in (executable, "Contents/Frameworks/libpb_mapper_ffi.dylib"):
        if not (app / name).is_file():
            raise RuntimeError(f"Missing release component: {name}")
    files = macho_files(app)
    minimum = tuple(map(int, info.get("LSMinimumSystemVersion", "10.15").split(".")))
    for path in files:
        arches = set(run("lipo", "-archs", path).split())
        # Flutter frameworks may be universal; every component must include
        # the requested architecture, and our executable and FFI are single-arch.
        if arch not in arches or (path in (app / executable, app / "Contents/Frameworks/libpb_mapper_ffi.dylib") and arches != {arch}):
            raise RuntimeError(f"Expected {arch} in {path.name}, got {arches}")
        for line in run("otool", "-L", path).splitlines():
            if line.startswith("\t"):
                dependency = line.strip().split(" (", 1)[0]
                if not dependency.startswith(("@rpath/", "@loader_path/", "@executable_path/",
                                              "/usr/lib/", "/System/Library/")):
                    raise RuntimeError(f"Nonportable dependency in {path.name}: {dependency}")
        for command in run("otool", "-l", path).split("Load command "):
            if re.search(r"\bcmd LC_(?:BUILD_VERSION|VERSION_MIN_MACOSX)\b", command):
                version = re.search(r"^\s*(?:minos|version) (\d+(?:\.\d+)+)$", command, re.MULTILINE)
                if version:
                    minimum = max(minimum, tuple(map(int, version[1].split("."))))
    if "@rpath/libpb_mapper_ffi.dylib" not in run("otool", "-L", app / executable):
        raise RuntimeError("Main executable does not load the bundled pb-mapper library via @rpath")
    # Flutter plugins may raise the supported OS above the Runner's deployment
    # target. Report the actual requirement instead of shipping a misleading plist.
    info["LSMinimumSystemVersion"] = ".".join(map(str, minimum))
    info_path.write_bytes(plistlib.dumps(info))
    return files, info


def notarize(path, auth, log_dir):
    print(f"Submitting {path.name} to Apple notarization…", flush=True)
    submission = subprocess.run(
        ["xcrun", "notarytool", "submit", str(path), *auth,
         "--wait", "--timeout", "2h", "--output-format", "json"],
        capture_output=True, text=True)
    # notarytool can return a nonzero exit code together with a useful JSON
    # submission ID (for example on rejection or timeout). Preserve that ID.
    try:
        result = json.loads(submission.stdout)
    except ValueError:
        raise RuntimeError(f"notarytool did not return a submission result: {submission.stderr}") from None
    (log_dir / f"{path.name}.notary.json").write_text(json.dumps(result, indent=2) + "\n")
    if submission.returncode or result.get("status") != "Accepted":
        if result.get("id") and result.get("status") == "Invalid":
            log = run("xcrun", "notarytool", "log", result["id"], *auth)
            (log_dir / f"{path.name}.notary-log.json").write_text(log)
        raise RuntimeError(f"Apple notarization was not accepted: {result.get('status')}; see {log_dir}")
    print(f"Apple accepted {path.name}: {result['id']}", flush=True)


def verify_mounted_app(image, work, expected_info, team, logs):
    """Validate the application users receive, after DMG creation and stapling."""
    mount = work / "mounted"
    mount.mkdir()
    run("hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount, image)
    try:
        app = mount / "pb-mapper.app"
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        for key in ("CFBundleIdentifier", "CFBundleExecutable", "CFBundleShortVersionString", "CFBundleVersion"):
            if info.get(key) != expected_info.get(key):
                raise RuntimeError(f"Mounted release has a mismatched {key}")
        report = run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
        signature = run("codesign", "--display", "--verbose=4", app)
        if f"TeamIdentifier={team}" not in signature or "Authority=Developer ID Application:" not in signature:
            raise RuntimeError("Mounted application is missing the expected Developer ID signature")
        report += signature
        report += stapler("validate", app)
        report += run("spctl", "--assess", "--type", "execute", "--verbose=4", app)
        if shutil.which("syspolicy_check"):
            report += run("syspolicy_check", "distribution", app)
        (logs / "mounted-app-verification.txt").write_text(report)
    finally:
        run("hdiutil", "detach", mount)


def package(args):
    identity = required("MACOS_SIGNING_IDENTITY")
    team = required("APPLE_TEAM_ID")
    output = args.output.resolve()
    if output.exists():
        raise RuntimeError(f"Refusing to overwrite existing artifact: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    logs = output.parent / (output.stem + "-verification")
    logs.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="pb-mapper-signing-", dir=os.environ.get("RUNNER_TEMP")) as temporary:
        work = Path(temporary)
        stage = work / "image"
        stage.mkdir()
        app = stage / "pb-mapper.app"
        run("ditto", args.app.resolve(), app)
        files, info = audit_bundle(app, args.arch)
        entitlements = plistlib.loads(args.entitlements.read_bytes())
        if entitlements.get("com.apple.security.get-task-allow"):
            raise RuntimeError("Release entitlements must not allow debugger attachment")
        with credentials(work) as (signing, auth):
            def sign(path, app_entitlements=False):
                options = ["--entitlements", str(args.entitlements.resolve())] if app_entitlements else []
                run("codesign", "--force", "--sign", identity, "--timestamp", "--options", "runtime",
                    *signing, *options, path)

            for path in files:
                if path != app / "Contents/MacOS" / info["CFBundleExecutable"]:
                    sign(path)
            bundles = [p for p in app.rglob("*") if p.is_dir() and not p.is_symlink()
                       and p.suffix in {".framework", ".app", ".xpc", ".appex"}]
            for path in sorted(bundles, key=lambda p: len(p.parts), reverse=True):
                sign(path)
            sign(app, app_entitlements=True)
            run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
            for path in [*files, app]:
                details = run("codesign", "--display", "--verbose=4", path)
                if (f"TeamIdentifier={team}" not in details
                        or "Authority=Developer ID Application:" not in details
                        or "Timestamp=" not in details or "(runtime)" not in details):
                    raise RuntimeError(f"Not a timestamped Developer ID signature for team {team}: {path.name}")
            archive = work / "pb-mapper-app.zip"
            run("ditto", "-c", "-k", "--keepParent", app, archive)
            notarize(archive, auth, logs)
            stapler("staple", app)
            stapler("validate", app)
            run("spctl", "--assess", "--type", "execute", "--verbose=2", app)
            (stage / "Applications").symlink_to("/Applications")
            candidate = work / output.name
            run("hdiutil", "create", "-volname", "pb-mapper", "-srcfolder", stage,
                "-format", "UDZO", candidate)
            run("codesign", "--sign", identity, "--timestamp", *signing, candidate)
            notarize(candidate, auth, logs)
            stapler("staple", candidate)
            stapler("validate", candidate)
            run("hdiutil", "verify", candidate)
            run("codesign", "--verify", "--strict", candidate)
            run("spctl", "--assess", "--type", "open", "--context", "context:primary-signature",
                "--verbose=2", candidate)
            verify_mounted_app(candidate, work, info, team, logs)
            run("ditto", candidate, output)
        report = {"version": info["CFBundleShortVersionString"], "build": info["CFBundleVersion"],
                  "minimum_macos": info["LSMinimumSystemVersion"], "team_id": team,
                  "architectures": [args.arch], "signed_macho_files": len(files),
                  "mounted_app_verified": True,
                  "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "size": output.stat().st_size}
        (logs / "release.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--arch", choices=("arm64", "x86_64"), required=True)
    parser.add_argument("--entitlements", type=Path,
                        default=Path(__file__).resolve().parents[2] / "ui/macos/Runner/Release.entitlements")
    try:
        package(parser.parse_args())
    except (RuntimeError, ValueError) as error:
        message = str(error)
        for name in ("MACOS_CERTIFICATE_PASSWORD", "MACOS_CERTIFICATE_P12_BASE64", "APPLE_NOTARY_PRIVATE_KEY"):
            value = os.environ.get(name)
            if value:
                message = message.replace(value, "[redacted]")
        raise SystemExit(message) from None

#!/usr/bin/env python3
"""Exercise real package signing with disposable Minisign keys."""

import argparse
import base64
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

import plugin_release


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, default=Path("plugins/hydra-youtube") / plugin_release.load("hydra-youtube").file_name())
    parser.add_argument("--validator", default="target/debug/hydra-plugin")
    parser.add_argument("--minisign", default="minisign")
    parser.add_argument("--hydra", default="target/debug/hydra")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="hydra-signing-e2e-") as temporary:
        root = Path(temporary)
        for name in ("publisher", "other"):
            subprocess.run([args.minisign, "-G", "-W", "-p", str(root / f"{name}.pub"),
                            "-s", str(root / f"{name}.key")], check=True, capture_output=True)
        secret = (root / "publisher.key").read_text()
        output = root / "signed.hyaplugin"
        original = args.package.read_bytes()
        command = [args.validator, "sign", str(args.package),
                   "--output", str(output), "--public-key", str(root / "publisher.pub"),
                   "--key-env", "HYDRA_PLUGIN_SIGNING_KEY"]

        def invoke(command, key, success):
            env = os.environ.copy()
            env["HYDRA_PLUGIN_SIGNING_KEY"] = key
            result = subprocess.run(command, env=env, text=True, capture_output=True)
            assert (result.returncode == 0) == success, result.stdout + result.stderr
            assert secret not in result.stdout + result.stderr
            return result.stdout + result.stderr

        assert "empty key environment variable" in invoke(command, "", False)
        assert not output.exists()
        malformed_public = root / "malformed.pub"
        malformed_public.write_text("invalid public key\n")
        malformed = command.copy()
        malformed[malformed.index("--public-key") + 1] = str(malformed_public)
        assert "public key" in invoke(malformed, secret, False)
        invoke(command, "invalid private key\n", False)
        for malformed_bytes in [b"\0" * 158, b"Ed\0\0B2"]:
            malformed_key = "untrusted comment: test key\n" + base64.b64encode(malformed_bytes).decode() + "\n"
            invoke(command, malformed_key, False)
        invalid_package = root / "invalid.hyaplugin"
        invalid_package.write_bytes(b"invalid package")
        invalid = command.copy()
        invalid[2] = str(invalid_package)
        invoke(invalid, secret, False)
        assert not output.exists()
        wrong = command.copy()
        wrong[wrong.index("--public-key") + 1] = str(root / "other.pub")
        invoke(wrong, secret, False)
        assert not output.exists()
        assert "Verified" in invoke(command, secret, True)
        assert args.package.read_bytes() == original
        with zipfile.ZipFile(output) as archive:
            (root / "SHA256SUMS").write_bytes(archive.read("SHA256SUMS"))
            (root / "SHA256SUMS.minisig").write_bytes(archive.read("SHA256SUMS.minisig"))
        subprocess.run([args.minisign, "-V", "-p", str(root / "publisher.pub"),
                        "-m", str(root / "SHA256SUMS")], check=True, capture_output=True)
        file_command = command[:-2] + ["--secret-key", str(root / "publisher.key")]
        assert "Verified" in invoke(file_command, "", True)
        profile_env = {**os.environ, "HYDRA_CONFIG_DIR": str(root / "profile"), "TERM": "xterm-256color"}
        profile_env.pop("NO_COLOR", None)

        def hydra(*arguments, success=True):
            result = subprocess.run([args.hydra, "plugin", *arguments], env=profile_env,
                                    capture_output=True, text=True)
            assert (result.returncode == 0) == success, result.stdout + result.stderr
            return result.stdout

        import json
        hydra("install", str(output), "--accept-permissions")
        info = hydra("info", "hydra.youtube")
        assert "Signed (verified)" in info and "Publisher fingerprint (SHA-256)" in info
        assert "\x1b" not in info
        metadata = json.loads(hydra("info", "hydra.youtube", "--json"))
        assert "Verified" in metadata["signing"]
        hydra("install", str(args.package), "--accept-permissions", "--accept-publisher-change")
        assert "Signature: Unsigned" in hydra("info", "hydra.youtube")
        hydra("install", str(output), "--accept-permissions", "--accept-publisher-change")
        development = root / "development"
        with zipfile.ZipFile(output) as archive:
            archive.extractall(development)
        hydra("install", str(development), "--accept-permissions")
        assert "Signature: Unsigned" in hydra("info", "hydra.youtube")
        hydra("install", str(output), "--accept-permissions")
        if os.name != "nt":
            import pty
            master, slave = pty.openpty()
            process = subprocess.Popen([args.hydra, "plugin", "info", "hydra.youtube"],
                                       env=profile_env, stdout=slave, stderr=subprocess.PIPE)
            assert process.wait(timeout=30) == 0
            import select
            chunks = []
            while select.select([master], [], [], 1)[0]:
                try:
                    chunk = os.read(master, 8192)
                except OSError:
                    break
                if not chunk:
                    break
                chunks.append(chunk)
            rendered = b"".join(chunks).decode()
            os.close(master)
            os.close(slave)
            assert "\x1b[32mSigned (verified)\x1b[0m" in rendered, repr(rendered)
            profile_env["NO_COLOR"] = "1"
            assert "\x1b" not in hydra("info", "hydra.youtube")
        signed_bytes = output.read_bytes()
        invoke(command, "", False)
        assert output.read_bytes() == signed_bytes
        with zipfile.ZipFile(output) as archive:
            entries = {name: archive.read(name) for name in archive.namelist()}
        entries["hydra-plugin.toml"] = entries["hydra-plugin.toml"].replace(b"Hydra Team", b"Impostor")
        entries["SHA256SUMS"] = "".join(
            f"{hashlib.sha256(data).hexdigest()}  {name}\n"
            for name, data in sorted(entries.items())
            if name not in {"SHA256SUMS", "SHA256SUMS.minisig"}
        ).encode()
        tampered = root / "tampered.hyaplugin"
        with zipfile.ZipFile(tampered, "w") as archive:
            for name, data in entries.items():
                archive.writestr(name, data)
        assert "signature does not verify" in invoke([args.validator, "validate", str(tampered)], "", False)
        signed_input = command.copy()
        signed_input[2] = str(output)
        signed_input[signed_input.index("--output") + 1] = str(root / "resigned.hyaplugin")
        assert "already declares" in invoke(signed_input, secret, False)
    print("PASS: native signing, Minisign compatibility, key failures, tampering, signed/unsigned info, development folders, and green terminal status")


if __name__ == "__main__":
    main()

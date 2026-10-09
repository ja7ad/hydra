import os
from pathlib import Path
import subprocess
import sys


def run(command, timeout=300):
    try:
        return subprocess.run(command, timeout=timeout, check=False).returncode
    except subprocess.TimeoutExpired:
        print("::error::Signature verification exceeded five minutes; inspect the last progress message", flush=True)
        return 1


if __name__ == "__main__":
    script = Path(__file__).with_name("verify-authenticode.ps1")
    sys.exit(run([
        "pwsh", "-NoProfile", "-NonInteractive", "-File", str(script),
        "-UnsignedDirectory", os.environ["UNSIGNED_DIRECTORY"],
        "-SignedDirectory", os.environ["SIGNED_DIRECTORY"],
        "-CertificateThumbprint", os.environ["CERTIFICATE_THUMBPRINT"],
        "-SigningPolicy", os.environ["SIGNING_POLICY"],
    ]))

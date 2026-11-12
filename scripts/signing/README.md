# Windows release signing

`release.yml` skips Windows Authenticode signing unless repository variable
`SIGNPATH_RELEASE_SIGNING_ENABLED` is exactly `true`. With signing disabled,
Windows application and FFI builds use their normal final artifact names,
without an `unsigned-` prefix. Release notes state
that Windows files are not Authenticode signed. Plugin publisher signatures
remain enabled independently of Authenticode.

When enabled, the release calls the reusable `sign-windows.yml` after the build
matrix and Windows FFI builds complete, using only the production certificate.
Both AMD64 and ARM64 pass through signing before release upload. Unsigned inputs
use `unsigned-` artifact names; publication selects only final artifacts.
Official plugins are not part of this pipeline: `plugins.yml` builds and
publishes them, and the release embeds those published packages unchanged, so
their checksums match `docs/plugins.json`. Signing failure blocks publication.

The release workflow no longer uses `SIGNPATH_TEST_SIGNING`. Test signing remains
available to callers of `sign-windows.yml` through its `test_signing` input for
onboarding; test certificates are not used by the release workflow.

Signing requires the repository secret `SIGNPATH_API_TOKEN`. Production also requires
`SIGNPATH_RELEASE_SIGNING_ENABLED=true` and a production
`SIGNPATH_CERTIFICATE_THUMBPRINT`, as described below. Production errors never
fall back to test signing or unsigned files.

SignPath Foundation restricts signing to one product name and version, so the
configuration takes a required `version` parameter, which the composite action
passes from the job's `VERSION`. Every executable and DLL Hydra builds embeds
`ProductName` "Hydra Download Manager" and the workspace `ProductVersion`
(`scripts/windows/version_resource.rs` for Rust; `VIAddVersionKey` in the NSIS
installer). Third-party binaries (`hydra-torrent-native.dll`, which statically
links libtorrent, and the PortableApps `HydraPortable.exe`) are deliberately
not matched, so they pass through unsigned.

Update the SignPath **hydra → hydra-files** artifact configuration to match
[`artifact-configuration.xml`](artifact-configuration.xml) before running this
pipeline: the rules now include every `.dll` and `.ps1`, including the FFI DLL
and NSIS `set-user-path.ps1`, as well as `.exe` files. Keep trusted build origin
verification enabled, and authorize the release workflow in SignPath if its
policy requires a workflow allowlist. The submission uses the pinned official
[SignPath action](https://github.com/SignPath/github-action-submit-signing-request).

Applications are rebuilt from the same release SHA with the released official
plugin packages embedded. Applications, portable launcher, PowerShell scripts and
exported uninstaller are signed before packaging; setup is signed last. FFI ZIPs
preserve their own library version, static libraries, headers and documentation.
FFI DLLs are staged as root-level `ffi-<index>-<name>.dll` files for signing, then
restored to their original archive paths. The index keeps identical DLL names in
different directories distinct and avoids relying on nested DLL signing rules.

Final artifact names retain `hydra-windows-amd64`, `hydra-windows-arm64` and
`libhydra-<target>`; installation scripts use
`windows-installation-scripts`. Release filenames keep their original names
without any `signed_` or `signed-` prefix, preserving existing download URLs.

Signed root `install.ps1` and `uninstall.ps1` are attached as release assets.
Repository/raw URLs continue to serve the committed scripts; signing does not
commit them back to the repository.

The historical `sign-windows-v1.1.0.yml` repair workflow is retained disabled
for reference. It has no push trigger, and its preflight job always skips, so
manual dispatch cannot perform signing. The instructions below describe its
original operation only.

## Windows v1.1.0 signing repair

The workflow rebuilds Windows applications from the pinned v1.1.0 commit. It
recovers the original torrent DLL from release run `37755183960`, checks it
against the released plugin, signs it, and regenerates the plugin's checksums
and Minisign publisher signature before compiling the applications that embed
it. These are new builds of v1.1.0, not byte-for-byte copies of the old EXEs.

## First run: Foundation onboarding

1. Accept your SignPath organization invitation, then confirm the CI user's
   email, in the order specified by the Foundation email.
2. Set repository secret `SIGNPATH_API_TOKEN` to the existing **CI builds**
   submitter's token. Keep `HYDRA_PLUGIN_SIGNING_KEY` configured; it regenerates
   the torrent plugin signature. Never put either value in Git.
3. Review and commit these files, then merge them into `main`. In GitHub Actions,
   select **Sign Windows v1.1.0 → Run workflow**, choose branch `main`, and leave
   `signing_policy` set to **test-signing**.
4. Inspect the successful workflow and SignPath signing requests. Share the
   workflow URL with Phillip and ask for the setup review and production
   certificate issuance/import. Test outputs must not replace release assets.

The public `test-certificate.cer` is downloaded from SignPath, has no private key,
and has SHA-1 thumbprint `4924FFE138E36949CA3EDDF03252903CDB65B2AE`. This is pinned
in the test workflow; no production thumbprint variable is needed for onboarding.
Verification adds this certificate to Local Machine's Trusted Root store only on
GitHub-hosted runners and removes it in a `finally` block. It still requires a
valid Authenticode signature, the exact certificate, a timestamp, and the complete
output file set. It never accepts a hash mismatch or simply ignores untrusted
signatures, and never installs test trust on a developer machine. Machine-store
import avoids an interactive current-user trust prompt on unattended Windows
runners. Verification logs each file before checking it and has a five-minute
CI timeout.

Manual test runs do not require an enable flag. To automatically test after
relevant pushes to `main`, set repository variable
`SIGNPATH_TEST_SIGNING_ENABLED=true`. Pushes always use **test-signing**. Tests
never run on pull requests or other branches. A variable change alone does not
trigger a workflow; use **Run workflow** after configuring it.

## Production activation

After SignPath Foundation issues/imports the production certificate:

1. Verify `release-signing` is valid. Keep approval and trusted origin verification
   enabled. Only approve requests from this reviewed workflow and source commit.
2. Set repository variable `SIGNPATH_CERTIFICATE_THUMBPRINT` to the production
   certificate's SHA-1 thumbprint: 40 hexadecimal characters without spaces. The
   workflow refuses to use the test certificate as the production certificate.
3. After validating production signing, set `SIGNPATH_RELEASE_SIGNING_ENABLED=true`
   to enable it for releases. Leave it unset or `false` while signing is unavailable.
4. Run the release workflow with the new configuration. Enabled signing failures
   block publication; they never fall back to unsigned output.

Each request uses `hydra-files` explicitly and submits loose files inside the
GitHub artifact ZIP. Compare that configuration with `artifact-configuration.xml`
in this directory. Nested release ZIPs are repacked locally; nested SignPath ZIP
rules are not needed. The native request includes the plugin authoring EXE because
this configuration requires at least one EXE.

The SignPath connector receives artifacts uploaded in the new workflow run. Its
origin identifies this signing repair workflow; the original run and released
source commit are checked explicitly. No origin metadata or environment IDs are
spoofed. SignPath must accept this recovery pipeline under the Foundation policy;
if it rejects recovered DLLs, obtain approval from SignPath or rebuild the native
DLL within the workflow. Do not disable production origin verification. Successful
test signing alone does not establish acceptance under production origin policy.

## Outputs and publication

Each architecture produces `hydra-windows-amd64` and `hydra-windows-arm64`, matching
the original Actions artifact names and retaining release filenames. Test outputs
contain `TEST-SIGNING-ONLY.txt` and put signed root scripts in the clearly marked
`test-remote-installers` directory; they never produce the production
`signed-remote-installers` artifact. Do not publish these test files or commit test
script signatures back to `main`.

The signed uninstaller is exported in a first NSIS pass and included in a second
pass; setup is signed last. The portable launcher and application executables
are signed before packing the portable ZIPs. Verification occurs before repacking.

The workflow leaves the published release intact. After a successful production
run, test installation, uninstallation, portable startup and torrent loading on
Windows x64 and ARM64 before replacing release files. Regenerate the shared
`hydra-official-plugins.zip` with both newly signed Windows plugin packages,
preserve its Linux and macOS packages, and regenerate release-wide `SHA256SUMS.txt`.
Do not upload individual torrent packages while leaving the official bundle stale.
The workflow summary reports per-architecture checksums for review only.

GitHub stores artifact names per run; these names do not overwrite the original
run artifacts. Original workflow artifacts must remain unexpired. Windows libhydra
FFI DLLs are outside this repair's scope and need their own artifact configuration.
No release assets are deleted, overwritten or published automatically.

## Root remote installation scripts

The run signs root `install.ps1` and `uninstall.ps1` from the checked-out workflow
commit, independently of the historical uninstaller script in the v1.1.0 archive.
Only production signing exposes them as `signed-remote-installers`. The
`**/install.ps1` rule from the tracked XML must be present in SignPath.

Copy verified production scripts back into the repository root without changing
their bytes, review and commit them yourself. CI never commits or pushes
signatures. Repository/raw URLs have the signatures only after that commit. Any
later script edit requires signing again. `irm ... | iex` evaluates text and does
not enforce Authenticode; download the script, check `Get-AuthenticodeSignature`,
and run the saved file under an appropriate execution policy when enforcement is
required.

A manual onboarding smoke request successfully signed `HydraPortable.exe`,
`install.ps1`, and `uninstall.ps1` with this test certificate:
[SignPath request](https://app.signpath.io/Web/e8665a13-7b53-4cc4-9819-7832845ae7f5/SigningRequests/58305a9b-d286-4eb8-91d4-ac8f837f9baf).
That request validates artifact rules only; it has no GitHub build-origin data.
The GitHub workflow must still complete before requesting the Foundation review.

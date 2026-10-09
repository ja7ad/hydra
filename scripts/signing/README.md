# Windows v1.1.0 signing repair

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
Verification adds this certificate to Current User's Trusted Root store only on
GitHub-hosted runners and removes it in a `finally` block. It still requires a
valid Authenticode signature, the exact certificate, a timestamp, and the complete
output file set. It never accepts a hash mismatch or simply ignores untrusted
signatures, and never installs test trust on a developer machine.

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
3. Set `SIGNPATH_RELEASE_SIGNING_ENABLED=true`.
4. Manually run the workflow on `main`, selecting **release-signing**. Production
   signing is never selected automatically by a push.

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

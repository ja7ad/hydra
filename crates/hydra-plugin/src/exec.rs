//! The `exec` host function: argument templates, program pinning and a
//! bounded, deadline-killed child process.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_plugin_api::limits::MAX_EXEC_OUTPUT;
use hya_plugin_api::manifest::is_refused_program;
use hya_plugin_api::{ErrorCode, ExecEntry, PluginError};
use serde::{Deserialize, Serialize};

use crate::package::sha256_hex;
use crate::runtime::CallCtl;

pub const COOKIE_FILE: &str = "{cookie_file}";
pub const DATA_DIR: &str = "{data}";
const STDERR_TAIL: usize = 2048;
const BUSY_SPAWN_RETRIES: usize = 10;
const KEPT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SYSTEMROOT",
    // Windows tools need these to find runtimes (`node` → `node.exe`) and profiles.
    "PATHEXT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

/// A resolved program as it was when the user approved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn candidates(name: &str) -> Vec<String> {
    if cfg!(windows) && !name.contains('.') {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    }
}

/// Finds `name` in `dirs`, in order.
pub fn find_program(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|d| candidates(name).into_iter().map(move |c| d.join(c)))
        .find(|p| is_executable(p))
}

/// Directories searched for a program: an explicit setting, `PATH`, then the
/// well-known tool locations.
pub fn search_dirs(setting_path: Option<&Path>) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    search_dirs_with_path(
        setting_path,
        std::env::var_os("PATH").as_deref(),
        home.as_deref().map(Path::new),
    )
}

/// Where per-user installers put tools: Deno, pipx and pip `--user`, Bun.
const HOME_TOOL_DIRS: [&str; 3] = [".deno/bin", ".local/bin", ".bun/bin"];

fn search_dirs_with_path(
    setting_path: Option<&Path>,
    path: Option<&OsStr>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = setting_path {
        match p.parent() {
            Some(parent) if p.is_file() => dirs.push(parent.to_path_buf()),
            _ => dirs.push(p.to_path_buf()),
        }
    }
    if let Some(path) = path {
        dirs.extend(std::env::split_paths(&path));
    }
    for extra in [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/bin",
        "/snap/bin",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    // A desktop launcher's PATH lacks what the user's shell profile adds.
    if let Some(home) = home {
        dirs.extend(HOME_TOOL_DIRS.iter().map(|dir| home.join(dir)));
    }
    dirs
}

/// Resolves and pins a program for a grant.
///
/// # Errors
/// `tool_missing` when it cannot be found or read.
pub fn pin(name: &str, dirs: &[PathBuf]) -> Result<Pin, PluginError> {
    if is_refused_program(name) {
        return Err(PluginError::new(
            ErrorCode::PermissionDenied,
            format!("`{name}` is a shell or interpreter"),
        ));
    }
    let path = find_program(name, dirs).ok_or_else(|| {
        PluginError::new(ErrorCode::ToolMissing, format!("`{name}` was not found"))
    })?;
    pin_path(path)
}

fn pin_path(path: PathBuf) -> Result<Pin, PluginError> {
    let missing = |e: std::io::Error| {
        PluginError::new(ErrorCode::ToolMissing, format!("{}: {e}", path.display()))
    };
    let meta = std::fs::metadata(&path).map_err(missing)?;
    let bytes = std::fs::read(&path).map_err(missing)?;
    Ok(Pin {
        size: meta.len(),
        mtime: mtime_of(&meta),
        sha256: sha256_hex(&bytes),
        path,
    })
}

/// Confirms the file on disk is still the one that was approved.
///
/// # Errors
/// `tool_missing` when it is gone or its hash changed.
pub fn verify(pin: &Pin) -> Result<(), PluginError> {
    let gone = |e: std::io::Error| {
        PluginError::new(
            ErrorCode::ToolMissing,
            format!("{}: {e}", pin.path.display()),
        )
    };
    let bytes = std::fs::read(&pin.path).map_err(gone)?;
    if sha256_hex(&bytes) == pin.sha256 {
        return Ok(());
    }
    Err(PluginError::new(
        ErrorCode::ToolMissing,
        format!(
            "`{}` changed since you approved it — approve again",
            pin.path.display()
        ),
    ))
}

/// Whether `args` satisfies `template`: literals match themselves, `*` one
/// argument not starting with `-`, `**` the rest.
pub fn matches_template(template: &[String], args: &[String]) -> bool {
    let mut i = 0;
    for (t, tpl) in template.iter().enumerate() {
        match tpl.as_str() {
            "**" => return t == template.len() - 1,
            "*" => match args.get(i) {
                Some(a) if !a.starts_with('-') => i += 1,
                _ => return false,
            },
            lit => {
                if args.get(i).map(String::as_str) != Some(lit) {
                    return false;
                }
                i += 1;
            }
        }
    }
    i == args.len()
}

/// Finds the first declared entry for `program` whose template admits `args`.
pub fn admit<'a>(
    entries: &'a [ExecEntry],
    program: &str,
    args: &[String],
) -> Option<&'a ExecEntry> {
    entries
        .iter()
        .find(|e| e.program == program && matches_template(&e.args, args))
}

/// Captured result of a finished child.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExecOutput {
    pub status: i32,
    pub stdout_b64: String,
    pub stderr: String,
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let mut over = false;
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_EXEC_OUTPUT.saturating_sub(out.len());
                    if n > room {
                        over = true;
                    }
                    out.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
        (out, over)
    })
}

/// Runs a pinned program with a scrubbed environment, killing it at the
/// call's deadline or on cancel.
///
/// # Errors
/// `tool_missing`, `tool_failed` (with a stderr tail), `deadline`, `cancelled`.
pub fn run(
    pin: &Pin,
    args: &[String],
    cwd: &Path,
    ctl: &Arc<CallCtl>,
    secrets: &[String],
) -> Result<ExecOutput, PluginError> {
    run_with_proxy(pin, args, cwd, ctl, secrets, None)
}

pub(crate) fn run_with_proxy(
    pin: &Pin,
    args: &[String],
    cwd: &Path,
    ctl: &Arc<CallCtl>,
    secrets: &[String],
    proxy: Option<&hya_net::Proxy>,
) -> Result<ExecOutput, PluginError> {
    let mut cmd = Command::new(&pin.path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        cmd.creation_flags(0x0800_0000);
    }
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in KEPT_ENV {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    // Finder's PATH can omit Homebrew even when the approved tool was found there.
    let path = std::env::join_paths(search_dirs(None))
        .map_err(|e| PluginError::new(ErrorCode::Internal, format!("tool PATH: {e}")))?;
    cmd.env("PATH", path);
    let mut redactions = secrets.to_vec();
    if let Some(proxy) = proxy {
        let mut address = url::Url::parse(&format!(
            "{}://{}:{}",
            proxy.kind.as_str(),
            proxy.host,
            proxy.port
        ))
        .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e.to_string()))?;
        if let Some(user) = &proxy.username {
            let _ = address.set_username(user);
        }
        if let Some(password) = &proxy.password {
            let _ = address.set_password(Some(password));
            redactions.push(password.clone());
        }
        let address = address.to_string();
        for key in ["http_proxy", "https_proxy", "all_proxy"] {
            cmd.env(key, &address);
        }
        redactions.push(address);
    }
    let secrets = &redactions;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        cmd.process_group(0);
    }
    let mut child = spawn_verified(pin, ctl, || cmd.spawn())?;
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));

    let mut status = None;
    let status = loop {
        if let Err(e) = ctl.check() {
            #[cfg(unix)]
            {
                // Descendants inherit the pipes, so killing only the parent cannot unblock readers.
                // SAFETY: the child leads its own process group; the negative PID targets that group.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = (out.join(), err.join());
            return Err(e);
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|e| PluginError::new(ErrorCode::Internal, format!("wait: {e}")))?;
        }
        if let Some(status) = status {
            if out.is_finished() && err.is_finished() {
                break status;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (stdout, over_out) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    if over_out {
        return Err(PluginError::new(
            ErrorCode::ToolFailed,
            "program output is over 16 MiB",
        ));
    }
    let mut stderr = redact(&String::from_utf8_lossy(&stderr), secrets);
    if stderr.len() > STDERR_TAIL {
        let cut = stderr.len() - STDERR_TAIL;
        let cut = (cut..stderr.len())
            .find(|&i| stderr.is_char_boundary(i))
            .unwrap_or(cut);
        stderr = stderr[cut..].to_string();
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        return Err(PluginError::new(
            ErrorCode::ToolFailed,
            format!("exited with {code}: {}", stderr.trim()),
        ));
    }
    let _ = SystemTime::now();
    Ok(ExecOutput {
        status: code,
        stdout_b64: hya_net::base64::encode(&redact_bytes(&stdout, secrets)),
        stderr,
    })
}

fn spawn_verified(
    pin: &Pin,
    ctl: &CallCtl,
    mut spawn: impl FnMut() -> std::io::Result<std::process::Child>,
) -> Result<std::process::Child, PluginError> {
    verify(pin)?;
    let mut retries = 0;
    loop {
        ctl.check()?;
        match spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && retries < BUSY_SPAWN_RETRIES =>
            {
                // A concurrent fork can briefly retain a writer until its exec closes the descriptor.
                retries += 1;
                std::thread::sleep(Duration::from_millis(10));
                ctl.check()?;
                verify(pin)?;
            }
            Err(error) => {
                return Err(PluginError::new(
                    ErrorCode::ToolMissing,
                    format!("{}: {error}", pin.path.display()),
                ));
            }
        }
    }
}

/// Replaces every secret's exact value with `***`.
pub fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(text.to_string(), |t, s| t.replace(s.as_str(), "***"))
}

fn redact_bytes(bytes: &[u8], secrets: &[String]) -> Vec<u8> {
    if secrets.iter().all(String::is_empty) {
        return bytes.to_vec();
    }
    redact(&String::from_utf8_lossy(bytes), secrets).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn finder_path_includes_homebrew_for_child_runtime_discovery() {
        let inherited = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let dirs = search_dirs_with_path(None, Some(&inherited), None);
        assert_eq!(
            &dirs[..2],
            &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(std::env::join_paths(&dirs).is_ok());
    }

    #[test]
    fn absent_path_still_searches_fallback_tool_folders() {
        let dirs = search_dirs_with_path(None, None, None);
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(std::env::join_paths(&dirs).is_ok());
    }

    #[test]
    fn per_user_tool_folders_are_searched_after_path_and_system_folders() {
        let inherited = std::env::join_paths(["/usr/bin"]).unwrap();
        let home = Path::new("/home/someone");
        let dirs = search_dirs_with_path(None, Some(&inherited), Some(home));
        assert_eq!(dirs[0], PathBuf::from("/usr/bin"));
        let position = |dir: &Path| dirs.iter().position(|d| d == dir).unwrap();
        for tool_dir in [".deno/bin", ".local/bin", ".bun/bin"] {
            assert!(position(&home.join(tool_dir)) > position(Path::new("/usr/local/bin")));
        }
        assert!(!search_dirs_with_path(None, Some(&inherited), None)
            .iter()
            .any(|dir| dir.ends_with(".deno/bin")));
    }

    #[test]
    fn the_real_search_path_ends_with_the_users_own_tool_folders() {
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return;
        };
        let dirs = search_dirs(None);
        assert_eq!(dirs.last(), Some(&Path::new(&home).join(".bun/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_installed_only_under_the_home_folder_is_found() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".deno/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let deno = bin.join("hydra-test-deno");
        std::fs::write(&deno, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&deno, std::fs::Permissions::from_mode(0o755)).unwrap();
        let with_home = search_dirs_with_path(None, None, Some(home.path()));
        assert_eq!(find_program("hydra-test-deno", &with_home), Some(deno));
        let without = search_dirs_with_path(None, None, None);
        assert_eq!(find_program("hydra-test-deno", &without), None);
    }

    #[test]
    fn templates_match_element_for_element() {
        assert!(matches_template(
            &s(&["-J", "--no-download", "*"]),
            &s(&["-J", "--no-download", "https://x"])
        ));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J"])));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J", "a", "b"])));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J", "-evil"])));
        assert!(!matches_template(&s(&["-J"]), &s(&["--other"])));
        assert!(matches_template(&s(&[]), &s(&[])));
        assert!(!matches_template(&s(&[]), &s(&["x"])));
    }

    #[test]
    fn double_star_takes_the_rest_but_only_last() {
        assert!(matches_template(&s(&["a", "**"]), &s(&["a"])));
        assert!(matches_template(&s(&["a", "**"]), &s(&["a", "-x", "y"])));
        assert!(!matches_template(&s(&["**", "a"]), &s(&["a"])));
    }

    #[test]
    fn admit_picks_the_entry_by_program_and_template() {
        let entries = vec![
            ExecEntry {
                program: "tool".into(),
                args: s(&["--version"]),
            },
            ExecEntry {
                program: "tool".into(),
                args: s(&["get", "*"]),
            },
        ];
        assert!(admit(&entries, "tool", &s(&["get", "u"])).is_some());
        assert!(admit(&entries, "tool", &s(&["rm", "u"])).is_none());
        assert!(admit(&entries, "other", &s(&["--version"])).is_none());
    }

    #[test]
    fn redaction_replaces_exact_values() {
        assert_eq!(redact("pw=hunter2 ok", &s(&["hunter2"])), "pw=*** ok");
        assert_eq!(redact("abc", &s(&[""])), "abc");
    }

    #[test]
    fn refused_programs_are_never_pinned() {
        assert_eq!(
            pin("bash", &search_dirs(None)).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            pin("definitely-not-a-real-tool-xyz", &search_dirs(None))
                .unwrap_err()
                .code,
            ErrorCode::ToolMissing
        );
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        #[cfg(target_os = "linux")]
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn tmp(tag: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("hya-exec-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        fn ctl(secs: u64) -> Arc<CallCtl> {
            CallCtl::new(Duration::from_secs(secs))
        }

        #[cfg(target_os = "linux")]
        fn busy_script(tag: &str) -> (PathBuf, Pin, std::fs::File) {
            let directory = tmp(tag);
            let path = script(&directory, "t", "echo approved");
            let pinned = pin_path(path.clone()).unwrap();
            let writer = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            (directory, pinned, writer)
        }

        #[test]
        fn runs_captures_and_scrubs_the_environment() {
            let d = tmp("run");
            script(
                &d,
                "t",
                "echo out; echo err >&2; echo \"s=$SECRET_VAR h=$HOME\"",
            );
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            std::env::set_var("SECRET_VAR", "leak");
            let out = run(&pinned, &[], &d, &ctl(10), &[]).unwrap();
            let body =
                String::from_utf8(hya_net::base64::decode(&out.stdout_b64).unwrap()).unwrap();
            assert!(body.contains("out"));
            assert!(body.contains("s= "), "{body}");
            assert_eq!(out.stderr.trim(), "err");
        }

        #[test]
        fn windows_runtime_lookup_variables_reach_the_child() {
            let dir = tmp("pathext");
            script(&dir, "t", "printf '%s|%s' \"$PATHEXT\" \"$USERPROFILE\"");
            let pinned = pin("t", std::slice::from_ref(&dir)).unwrap();
            std::env::set_var("PATHEXT", ".COM;.EXE");
            std::env::set_var("USERPROFILE", "/home/hydra");
            let output = run(&pinned, &[], &dir, &ctl(10), &[]).unwrap();
            let body =
                String::from_utf8(hya_net::base64::decode(&output.stdout_b64).unwrap()).unwrap();
            assert_eq!(body, ".COM;.EXE|/home/hydra");
        }

        #[test]
        fn child_path_keeps_inherited_priority_and_adds_tool_folders() {
            let dir = tmp("child-path");
            script(&dir, "t", "printf '%s' \"$PATH\"");
            let pinned = pin("t", std::slice::from_ref(&dir)).unwrap();
            let output = run(&pinned, &[], &dir, &ctl(10), &[]).unwrap();
            let path =
                String::from_utf8(hya_net::base64::decode(&output.stdout_b64).unwrap()).unwrap();
            let dirs: Vec<_> = std::env::split_paths(&path).collect();
            if let Some(inherited) = std::env::var_os("PATH") {
                let inherited: Vec<_> = std::env::split_paths(&inherited).collect();
                assert!(dirs.starts_with(&inherited));
            }
            assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
            assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        }

        #[test]
        fn explicit_proxy_reaches_backend_and_credentials_are_redacted() {
            let dir = tmp("proxy");
            script(&dir,"t","test -n \"$https_proxy\" || exit 2; echo \"$https_proxy\" >&2; echo \"$http_proxy\"");
            let pinned = pin("t", std::slice::from_ref(&dir)).unwrap();
            let proxy = hya_net::Proxy::parse("http://user:private@127.0.0.1:1234").unwrap();
            let output = run_with_proxy(&pinned, &[], &dir, &ctl(10), &[], Some(&proxy)).unwrap();
            assert!(!output.stderr.contains("private"));
            assert!(
                !String::from_utf8(hya_net::base64::decode(&output.stdout_b64).unwrap())
                    .unwrap()
                    .contains("private")
            );
        }
        #[test]
        fn nonzero_exit_is_tool_failed_with_a_redacted_tail() {
            let d = tmp("fail");
            script(&d, "t", "echo oops-sekret >&2; exit 3");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let e = run(&pinned, &[], &d, &ctl(10), &["sekret".into()]).unwrap_err();
            assert_eq!(e.code, ErrorCode::ToolFailed, "{e:?}");
            assert!(
                e.message.contains("exited with 3") && e.message.contains("oops-***"),
                "{}",
                e.message
            );
        }

        #[test]
        fn a_child_past_the_deadline_is_killed() {
            let d = tmp("slow");
            script(&d, "t", "sleep 30");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let started = std::time::Instant::now();
            let e = run(
                &pinned,
                &[],
                &d,
                &CallCtl::new(Duration::from_millis(150)),
                &[],
            )
            .unwrap_err();
            assert_eq!(e.code, ErrorCode::Deadline);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn deadline_kills_descendants_after_the_parent_exits() {
            let d = tmp("orphan-pipes");
            script(&d, "t", "sleep 30 &\nexit 0");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let started = std::time::Instant::now();
            let e = run(
                &pinned,
                &[],
                &d,
                &CallCtl::new(Duration::from_millis(150)),
                &[],
            )
            .unwrap_err();
            assert_eq!(e.code, ErrorCode::Deadline);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn cancel_kills_the_child() {
            let d = tmp("cancel");
            script(&d, "t", "sleep 30");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let c = ctl(60);
            let c2 = c.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                c2.cancel();
            });
            let started = std::time::Instant::now();
            assert_eq!(
                run(&pinned, &[], &d, &c, &[]).unwrap_err().code,
                ErrorCode::Cancelled
            );
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn a_changed_binary_is_refused_until_repinned() {
            let d = tmp("pin");
            let p = script(&d, "t", "echo one");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            verify(&pinned).unwrap();
            script(&d, "t", "echo two-two-two");
            let e = run(&pinned, &[], &d, &ctl(10), &[]).unwrap_err();
            assert_eq!(e.code, ErrorCode::ToolMissing);
            assert!(e.message.contains("approve again"));
            let again = pin_path(p).unwrap();
            let output = run(&again, &[], &d, &ctl(10), &[]).unwrap();
            assert_eq!(
                hya_net::base64::decode(&output.stdout_b64).unwrap(),
                b"two-two-two\n"
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn a_temporarily_busy_executable_runs_after_the_writer_closes() {
            let (d, pinned, writer) = busy_script("busy-released");
            let mut writer = Some(writer);
            let mut command = Command::new(&pinned.path);
            let mut child = spawn_verified(&pinned, &ctl(10), || {
                let result = command.spawn();
                if writer.is_some() {
                    assert_eq!(
                        result.as_ref().unwrap_err().kind(),
                        std::io::ErrorKind::ExecutableFileBusy
                    );
                    drop(writer.take());
                }
                result
            })
            .unwrap();
            assert!(child.wait().unwrap().success());
            let output = run(&pinned, &[], &d, &ctl(10), &[]).unwrap();
            assert_eq!(
                hya_net::base64::decode(&output.stdout_b64).unwrap(),
                b"approved\n"
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn an_executable_that_stays_busy_has_bounded_retries() {
            let (d, pinned, _writer) = busy_script("busy-persistent");
            assert_eq!(
                Command::new(&pinned.path).spawn().unwrap_err().kind(),
                std::io::ErrorKind::ExecutableFileBusy
            );
            let started = std::time::Instant::now();
            let error = run(&pinned, &[], &d, &ctl(10), &[]).unwrap_err();
            assert_eq!(error.code, ErrorCode::ToolMissing);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn busy_retry_refuses_a_changed_executable_before_it_can_run() {
            let (_, pinned, mut writer) = busy_script("busy-changed");
            let mut command = Command::new(&pinned.path);
            let mut attempts = 0;
            let error = spawn_verified(&pinned, &ctl(10), || {
                attempts += 1;
                let result = command.spawn();
                assert_eq!(
                    result.as_ref().unwrap_err().kind(),
                    std::io::ErrorKind::ExecutableFileBusy
                );
                writer.write_all(b"#!/bin/sh\necho changed!\n").unwrap();
                result
            })
            .unwrap_err();
            assert_eq!(attempts, 1);
            assert_eq!(error.code, ErrorCode::ToolMissing);
            assert!(error.message.contains("approve again"), "{error:?}");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn busy_retry_obeys_deadline_and_cancellation() {
            let (d, pinned, _writer) = busy_script("busy-controls");
            let control = CallCtl::new(Duration::from_millis(20));
            assert_eq!(
                run(&pinned, &[], &d, &control, &[]).unwrap_err().code,
                ErrorCode::Deadline
            );
            let control = ctl(10);
            let mut command = Command::new(&pinned.path);
            assert_eq!(
                spawn_verified(&pinned, &control, || {
                    let result = command.spawn();
                    control.cancel();
                    result
                })
                .unwrap_err()
                .code,
                ErrorCode::Cancelled
            );
        }

        #[test]
        fn a_touched_but_identical_binary_still_passes() {
            let d = tmp("touch");
            let p = script(&d, "t", "echo one");
            let mut pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            pinned.mtime += 100;
            verify(&pinned).unwrap();
            pinned.sha256 = "0".repeat(64);
            assert!(verify(&pinned).is_err());
            std::fs::remove_file(p).unwrap();
            assert_eq!(verify(&pinned).unwrap_err().code, ErrorCode::ToolMissing);
        }

        #[test]
        fn search_prefers_the_setting_directory() {
            let d = tmp("search");
            script(&d, "mytool", "true");
            let dirs = search_dirs(Some(&d));
            assert_eq!(find_program("mytool", &dirs), Some(d.join("mytool")));
            assert_eq!(search_dirs(Some(&d.join("mytool")))[0], d);
        }
    }
}

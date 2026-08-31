//! Shell completion scripts.
//!
//! `hydra completions <shell>` prints a script to stdout, generated from the
//! same `clap::Command` the parser itself builds — a flag added to `cli.rs`
//! is picked up here for free, with nothing to keep in sync by hand.
//! `hydra install-completions [shell]` goes one step further: it picks a
//! destination for that shell, writes the file, and reports the one
//! remaining manual step (if any) needed to make the shell load it.
//!
//! That "if any" is practical: Fish scans its completions directory
//! unconditionally, whereas Bash and Zsh may require configuring completion
//! paths or packages.
//!
//! | Shell | User destination | Auto-loaded? |
//! |---|---|---|
//! | bash | `$XDG_DATA_HOME/bash-completion/completions/hydra` | only with `bash-completion` (v2) installed |
//! | zsh | `~/.zfunc/_hydra` | only once `~/.zfunc` is on `$fpath` before `compinit` |
//! | fish | `$XDG_CONFIG_HOME/fish/completions/hydra.fish` | yes |
//! | elvish | `$XDG_CONFIG_HOME/elvish/lib/hydra-completion.elv` | no, needs `use` in `rc.elv` |
//! | powershell | `$XDG_CONFIG_HOME/powershell/hydra-completion.ps1` | no, needs dot-sourcing from `$PROFILE` |
//!
//! Every path and every script is named for the command it completes, which
//! is why [`DEFAULT_BIN`] is a parameter rather than a constant in the
//! generators: the installers also put the CLI on `$PATH` as `hya`, and a
//! script generated for `hydra` never fires for that name.

use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::path::PathBuf;

use crate::cli::Cli;

/// The command name completions are generated for unless another is asked
/// for. The other one in practice is `hya`, the short name every installer
/// links next to `hydra`.
pub const DEFAULT_BIN: &str = "hydra";

/// Render `shell`'s completion script for the command `bin`, from the live
/// `Cli` definition.
pub fn render(shell: Shell, bin: &str) -> String {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    generate(shell, &mut cmd, bin, &mut buf);
    String::from_utf8(buf).expect("clap_complete generators always emit UTF-8")
}

/// First directory named by `$<var>`, falling back to `$HOME/<home_rel>`.
fn xdg_dir(var: &str, home_rel: &str) -> PathBuf {
    std::env::var(var).map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(home_rel)
    })
}

/// Guess the caller's shell from `$SHELL`, for `install-completions` run with
/// no explicit argument. `None` when `$SHELL` is unset or unrecognised —
/// callers should then ask rather than guess wrong.
pub fn detect_shell() -> Option<Shell> {
    let path = std::env::var("SHELL").ok()?;
    let name = path.rsplit('/').next().unwrap_or(&path);
    match name {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    }
}

/// Where a completion script goes, and what is still needed (if anything) to
/// make the shell actually load it.
pub struct Destination {
    pub path: PathBuf,
    /// `None` when the directory is unconditionally auto-scanned; `Some(step)`
    /// names the one thing left to do otherwise.
    pub remaining_step: Option<String>,
}

/// Pick the destination for `shell`'s script for the command `bin`. `system`
/// selects the machine-wide location (typically requires root) over the
/// current user's.
pub fn destination(shell: Shell, system: bool, bin: &str) -> Destination {
    match shell {
        Shell::Bash if system => Destination {
            path: PathBuf::from(format!("/usr/share/bash-completion/completions/{bin}")),
            remaining_step: Some(
                "requires the bash-completion package (v2); with it installed, this \
                 directory is scanned automatically on the next shell start"
                    .to_string(),
            ),
        },
        Shell::Bash => Destination {
            path: xdg_dir("XDG_DATA_HOME", ".local/share")
                .join(format!("bash-completion/completions/{bin}")),
            remaining_step: Some(
                "requires the bash-completion package (v2) to be installed and sourced; \
                 with it, this file is picked up on the next shell start"
                    .to_string(),
            ),
        },
        Shell::Zsh if system => Destination {
            path: PathBuf::from(format!("/usr/share/zsh/site-functions/_{bin}")),
            remaining_step: Some(
                "on the default zsh fpath on most systems; start a new shell (or run \
                 `compinit`) to pick it up"
                    .to_string(),
            ),
        },
        Shell::Zsh => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            Destination {
                path: PathBuf::from(home).join(format!(".zfunc/_{bin}")),
                remaining_step: Some(
                    "add `fpath+=(~/.zfunc)` to ~/.zshrc BEFORE the `autoload -Uz compinit \
                     && compinit` line, then start a new shell"
                        .to_string(),
                ),
            }
        }
        Shell::Fish => {
            let dir = if system {
                PathBuf::from("/usr/share/fish/vendor_completions.d")
            } else {
                xdg_dir("XDG_CONFIG_HOME", ".config").join("fish/completions")
            };
            Destination {
                path: dir.join(format!("{bin}.fish")),
                // fish scans this directory itself; nothing else to do.
                remaining_step: None,
            }
        }
        Shell::Elvish => {
            let dir = if system {
                PathBuf::from("/usr/share/elvish/lib")
            } else {
                xdg_dir("XDG_CONFIG_HOME", ".config").join("elvish/lib")
            };
            Destination {
                path: dir.join(format!("{bin}-completion.elv")),
                remaining_step: Some(format!(
                    "add `use {bin}-completion` to rc.elv (run `edit:rc-path` in elvish to \
                     find it)"
                )),
            }
        }
        Shell::PowerShell => {
            let dir = if system {
                PathBuf::from("/usr/local/share/powershell/Modules")
            } else {
                xdg_dir("XDG_CONFIG_HOME", ".config").join("powershell")
            };
            Destination {
                path: dir.join(format!("{bin}-completion.ps1")),
                remaining_step: Some(
                    "add `. <path>` to $PROFILE (run `echo $PROFILE` in pwsh to find it)"
                        .to_string(),
                ),
            }
        }
        // `Shell` is `#[non_exhaustive]`: future shell variants land here
        // rather than failing to compile against a newer clap_complete.
        _ => Destination {
            path: PathBuf::from(format!("{bin}-completion.{shell}")),
            remaining_step: Some(
                "unrecognised shell variant; consult its own completion documentation".to_string(),
            ),
        },
    }
}

/// Write `shell`'s completion script for the command `bin` to its
/// destination, creating parent directories as needed. Under `dry_run`,
/// computes and returns the `Destination` without touching the filesystem.
pub fn install(
    shell: Shell,
    system: bool,
    dry_run: bool,
    bin: &str,
) -> Result<Destination, String> {
    let dest = destination(shell, system, bin);
    if !dry_run {
        if let Some(parent) = dest.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&dest.path, render(shell, bin))
            .map_err(|e| format!("could not write {}: {e}", dest.path.display()))?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_renders_a_nonempty_script() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            let script = render(shell, DEFAULT_BIN);
            assert!(
                !script.trim().is_empty(),
                "{shell} produced an empty completion script"
            );
            assert!(
                script.contains("hydra"),
                "{shell} script does not mention the binary name"
            );
        }
    }

    #[test]
    fn an_alias_gets_its_own_script_and_its_own_destination() {
        // A completion script names the command it completes, so `hya` needs
        // its own — the `hydra` one would never fire for it, and would
        // overwrite it if they shared a path.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = render(shell, "hya");
            assert!(
                script.contains("hya"),
                "{shell} script for hya does not mention it"
            );
            assert_ne!(
                destination(shell, false, "hya").path,
                destination(shell, false, DEFAULT_BIN).path,
                "{shell}: hya must not share hydra's completion path"
            );
        }
    }

    #[test]
    fn fish_needs_no_remaining_step_but_others_do() {
        assert!(destination(Shell::Fish, false, DEFAULT_BIN)
            .remaining_step
            .is_none());
        assert!(destination(Shell::Bash, false, DEFAULT_BIN)
            .remaining_step
            .is_some());
        assert!(destination(Shell::Zsh, false, DEFAULT_BIN)
            .remaining_step
            .is_some());
        assert!(destination(Shell::Elvish, false, DEFAULT_BIN)
            .remaining_step
            .is_some());
        assert!(destination(Shell::PowerShell, false, DEFAULT_BIN)
            .remaining_step
            .is_some());
    }

    #[test]
    fn system_and_user_destinations_differ() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let user = destination(shell, false, DEFAULT_BIN);
            let system = destination(shell, true, DEFAULT_BIN);
            assert_ne!(
                user.path, system.path,
                "{shell}: --system must pick a different path than the user default"
            );
        }
    }

    #[test]
    fn dry_run_reports_a_destination_without_writing() {
        let tmp = std::env::temp_dir().join(format!(
            "hydra-completions-test-{}-{}",
            std::process::id(),
            "dry"
        ));
        // Isolate from the real home directory so this test cannot touch it.
        std::env::set_var("HOME", &tmp);
        std::env::remove_var("XDG_DATA_HOME");
        let dest = install(Shell::Bash, false, true, DEFAULT_BIN).expect("dry run must not error");
        assert!(
            !dest.path.exists(),
            "dry_run must not create {}",
            dest.path.display()
        );
    }

    #[test]
    fn install_actually_writes_the_script() {
        let tmp = std::env::temp_dir().join(format!(
            "hydra-completions-test-{}-{}",
            std::process::id(),
            "write"
        ));
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        let dest = install(Shell::Fish, false, false, DEFAULT_BIN).expect("install must succeed");
        let contents = std::fs::read_to_string(&dest.path).expect("script must be readable back");
        assert!(contents.contains("hydra"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_shell_reads_the_basename_of_dollar_shell() {
        std::env::set_var("SHELL", "/usr/bin/zsh");
        assert_eq!(detect_shell(), Some(Shell::Zsh));
        std::env::set_var("SHELL", "/bin/fish");
        assert_eq!(detect_shell(), Some(Shell::Fish));
        std::env::set_var("SHELL", "/bin/tcsh");
        assert_eq!(
            detect_shell(),
            None,
            "an unsupported shell must not guess wrong"
        );
        std::env::remove_var("SHELL");
        assert_eq!(detect_shell(), None);
    }
}

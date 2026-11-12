//! The host capabilities the resolver uses, behind a seam tests can fake.
use std::collections::BTreeMap;

use hya_plugin_sdk::{ExecOutput, Host, Result, Value};

pub trait Env {
    fn exec(&self, program: &str, args: &[String]) -> Result<ExecOutput>;
    fn settings(&self) -> Result<BTreeMap<String, Value>>;
    fn log(&self, level: &str, message: &str);
    fn call(&self, name: &str, request: serde_json::Value) -> Result<serde_json::Value>;
}

impl Env for Host {
    fn exec(&self, program: &str, args: &[String]) -> Result<ExecOutput> {
        Host::exec(self, program, args)
    }

    fn settings(&self) -> Result<BTreeMap<String, Value>> {
        Host::settings(self)
    }

    fn log(&self, level: &str, message: &str) {
        let _ = Host::log(self, level, message);
    }

    fn call(&self, name: &str, request: serde_json::Value) -> Result<serde_json::Value> {
        Host::call(self, name, &request)
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use hya_plugin_sdk::{ErrorCode, PluginError};

    use super::*;

    /// Replays scripted `exec` and host-call replies and records what was asked.
    #[derive(Default)]
    pub struct FakeEnv {
        pub settings: BTreeMap<String, Value>,
        pub exec_replies: RefCell<VecDeque<Result<ExecOutput>>>,
        pub call_replies: RefCell<VecDeque<Result<serde_json::Value>>>,
        pub executed: RefCell<Vec<Vec<String>>>,
        pub called: RefCell<Vec<(String, serde_json::Value)>>,
        pub logs: RefCell<Vec<(String, String)>>,
    }

    pub fn output(stdout: &str, stderr: &str) -> Result<ExecOutput> {
        use base64::Engine;
        Ok(ExecOutput {
            status: 0,
            stdout_b64: base64::engine::general_purpose::STANDARD.encode(stdout),
            stderr: stderr.into(),
        })
    }

    pub fn failure(code: ErrorCode, message: &str) -> Result<ExecOutput> {
        Err(PluginError::new(code, message))
    }

    impl FakeEnv {
        pub fn with_exec(replies: impl IntoIterator<Item = Result<ExecOutput>>) -> Self {
            Self {
                exec_replies: RefCell::new(replies.into_iter().collect()),
                ..Self::default()
            }
        }

        pub fn set(mut self, key: &str, value: Value) -> Self {
            self.settings.insert(key.into(), value);
            self
        }

        pub fn replying(self, replies: impl IntoIterator<Item = serde_json::Value>) -> Self {
            self.call_replies
                .borrow_mut()
                .extend(replies.into_iter().map(Ok));
            self
        }

        pub fn logged(&self, level: &str, needle: &str) -> bool {
            self.logs
                .borrow()
                .iter()
                .any(|(l, m)| l == level && m.contains(needle))
        }
    }

    impl Env for FakeEnv {
        fn exec(&self, _: &str, args: &[String]) -> Result<ExecOutput> {
            self.executed.borrow_mut().push(args.to_vec());
            self.exec_replies
                .borrow_mut()
                .pop_front()
                .expect("an exec reply was scripted")
        }

        fn settings(&self) -> Result<BTreeMap<String, Value>> {
            Ok(self.settings.clone())
        }

        fn log(&self, level: &str, message: &str) {
            self.logs.borrow_mut().push((level.into(), message.into()));
        }

        fn call(&self, name: &str, request: serde_json::Value) -> Result<serde_json::Value> {
            self.called.borrow_mut().push((name.into(), request));
            self.call_replies
                .borrow_mut()
                .pop_front()
                .expect("a host-call reply was scripted")
        }
    }
}

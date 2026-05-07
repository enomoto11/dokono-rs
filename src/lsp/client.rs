//! Synchronous client that drives rust-analyzer over stdio.
//!
//! ## Readiness model
//! Reads `experimental/serverStatus` (a rust-analyzer extension); `quiescent: true`
//! means VFS scan, cargo metadata, proc-macro loading, and cache priming have all
//! completed. We track `(quiescent, generation)` under a mutex+condvar so callers
//! can wait deterministically for a fresh `quiescent: true` after a request. This
//! removes the need for fixed sleeps and bounded retry counts when handling
//! `-32801 ContentModified`.

use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, BufWriter};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

/// LSP `ContentModified` — server snapshot changed mid-request; spec says to retry.
const ERR_CONTENT_MODIFIED: i64 = -32801;

use crate::lsp::protocol;

#[derive(Default, Debug)]
struct QuiescentState {
    quiescent: bool,
    health: String,
    /// Bumped on every `experimental/serverStatus` notification.
    generation: u64,
}

type SharedState = Arc<(Mutex<QuiescentState>, Condvar)>;

pub struct Client {
    child: Option<Child>,
    stdin: BufWriter<ChildStdin>,
    next_id: AtomicI64,
    responses: Receiver<Value>,
    state: SharedState,
    reader_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
}

impl Client {
    /// Spawn rust-analyzer (must be on `$PATH`).
    ///
    /// We deliberately do **not** set `current_dir` to the target workspace: doing so
    /// makes rustup's proxy resolve the toolchain via the workspace's
    /// `rust-toolchain.toml`, and if that toolchain lacks rust-analyzer the proxy
    /// dies with "Unknown binary". Workspace info travels via `rootUri` instead.
    pub fn spawn(_workspace: &Path) -> Result<Self> {
        let mut child = Command::new("rust-analyzer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn rust-analyzer (is it on PATH?)")?;

        let stdin = child.stdin.take().context("rust-analyzer stdin missing")?;
        let stdout = child
            .stdout
            .take()
            .context("rust-analyzer stdout missing")?;
        let stderr = child
            .stderr
            .take()
            .context("rust-analyzer stderr missing")?;

        let (resp_tx, resp_rx) = channel::<Value>();
        let state: SharedState = Arc::new((Mutex::new(QuiescentState::default()), Condvar::new()));
        let state_clone = Arc::clone(&state);

        let reader_thread = thread::spawn(move || {
            let verbose = std::env::var_os("DOKONO_VERBOSE").is_some();
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = protocol::read_message(&mut reader) {
                // Response = id present, method absent.
                let is_response = msg.get("id").is_some() && msg.get("method").is_none();
                if is_response {
                    if resp_tx.send(msg).is_err() {
                        break;
                    }
                } else {
                    handle_server_status(&state_clone, &msg);
                    if verbose {
                        log_notification(&msg);
                    }
                }
            }
        });

        // Drain stderr to prevent the pipe from blocking; mirror it under DOKONO_VERBOSE.
        let verbose = std::env::var_os("DOKONO_VERBOSE").is_some();
        let stderr_thread = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if verbose {
                    eprintln!("[ra-stderr] {line}");
                }
            }
        });

        Ok(Self {
            child: Some(child),
            stdin: BufWriter::new(stdin),
            next_id: AtomicI64::new(1),
            responses: resp_rx,
            state,
            reader_thread: Some(reader_thread),
            stderr_thread: Some(stderr_thread),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn current_gen(&self) -> u64 {
        self.state
            .0
            .lock()
            .expect("state mutex poisoned")
            .generation
    }

    /// Block until a `quiescent: true` notification with `generation > prev_gen` arrives.
    /// Pass `0` for initial readiness; pass `current_gen()` (snapshotted before sending)
    /// when retrying a `-32801` response.
    pub fn wait_for_quiescent_after(&self, prev_gen: u64) -> Result<u64> {
        let (lock, cvar) = &*self.state;
        let mut guard = lock.lock().expect("state mutex poisoned");
        loop {
            if guard.quiescent && guard.generation > prev_gen {
                return Ok(guard.generation);
            }
            guard = cvar.wait(guard).expect("state mutex poisoned");
        }
    }

    /// Send a request and wait for the matching response, skipping unrelated ids.
    /// On `ContentModified` (-32801) wait for the next `quiescent: true` and retry.
    pub fn request<P, R>(&mut self, method: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params_value = serde_json::to_value(params)
            .with_context(|| format!("serialize params for `{method}`"))?;

        loop {
            let gen_before = self.current_gen();
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let msg = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params_value,
            });
            protocol::write_message(&mut self.stdin, &msg)
                .with_context(|| format!("failed to send LSP request `{method}`"))?;

            match self.recv_response_for(id)? {
                Outcome::Ok(value) => {
                    return serde_json::from_value(value)
                        .with_context(|| format!("failed to deserialize result of `{method}`"));
                }
                Outcome::Err { code, .. } if code == ERR_CONTENT_MODIFIED => {
                    self.wait_for_quiescent_after(gen_before)?;
                    continue;
                }
                Outcome::Err { code, message } => {
                    bail!("LSP error for `{method}`: code={code} message={message}");
                }
            }
        }
    }

    fn recv_response_for(&self, id: i64) -> Result<Outcome> {
        loop {
            let response = self
                .responses
                .recv()
                .context("rust-analyzer closed stdout before response")?;
            if response.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(err) = response.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)")
                    .to_string();
                return Ok(Outcome::Err { code, message });
            }
            let result = response.get("result").cloned().unwrap_or(Value::Null);
            return Ok(Outcome::Ok(result));
        }
    }

    pub fn notify<P>(&mut self, method: &str, params: P) -> Result<()>
    where
        P: Serialize,
    {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        protocol::write_message(&mut self.stdin, &msg)
            .with_context(|| format!("failed to send LSP notification `{method}`"))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Failsafe; callers should normally `shutdown` + `exit` before dropping.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(t) = self.reader_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.stderr_thread.take() {
            let _ = t.join();
        }
    }
}

enum Outcome {
    Ok(Value),
    Err { code: i64, message: String },
}

fn handle_server_status(state: &SharedState, msg: &Value) {
    if msg.get("method").and_then(Value::as_str) != Some("experimental/serverStatus") {
        return;
    }
    let params = match msg.get("params") {
        Some(p) => p,
        None => return,
    };
    let quiescent = params
        .get("quiescent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let health = params
        .get("health")
        .and_then(Value::as_str)
        .unwrap_or("ok")
        .to_string();

    let (lock, cvar) = &**state;
    let mut guard = lock.lock().expect("state mutex poisoned");
    guard.quiescent = quiescent;
    guard.health = health;
    guard.generation += 1;
    drop(guard);
    cvar.notify_all();
}

fn log_notification(msg: &Value) {
    let method = match msg.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => return,
    };
    match method {
        "$/progress" => {
            // Skip `report` events — there are thousands of them per index pass.
            let kind = msg
                .get("params")
                .and_then(|p| p.get("value"))
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            if kind != "begin" && kind != "end" {
                return;
            }
            let token = msg
                .get("params")
                .and_then(|p| p.get("token"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            eprintln!("$/progress token={token:?} kind={kind}");
        }
        "experimental/serverStatus" => {
            let q = msg
                .get("params")
                .and_then(|p| p.get("quiescent"))
                .and_then(Value::as_bool);
            let h = msg
                .get("params")
                .and_then(|p| p.get("health"))
                .and_then(Value::as_str);
            eprintln!("experimental/serverStatus quiescent={q:?} health={h:?}");
        }
        _ => {}
    }
}

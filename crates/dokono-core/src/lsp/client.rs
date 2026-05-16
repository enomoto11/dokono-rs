//! Client for rust-analyzer over stdio, built on `async-lsp`.
//!
//! Public API is sync: the tokio runtime lives inside `Client`, and every
//! request goes through `block_on`. Concurrency for batched requests is
//! exposed via `request_async` + `wait_all` so callers can fan many requests
//! into rust-analyzer before awaiting any.
//!
//! Readiness uses rust-analyzer's `experimental/serverStatus` notification —
//! `quiescent: true` means VFS scan, cargo metadata, proc-macro loading, and
//! cache priming are done. Tracked as `(quiescent, generation)` via a
//! `tokio::sync::watch` so `-32801 ContentModified` retries can wait
//! deterministically.

use anyhow::{Context, Result, anyhow, bail};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::{ErrorCode, MainLoop, ServerSocket};
use futures::future::{BoxFuture, FutureExt};
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use serde::{Deserialize, Serialize};
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

#[derive(Default, Clone, Debug)]
struct QuiescentState {
    quiescent: bool,
    generation: u64,
}

struct ClientState {
    generation: u64,
    quiescent_tx: watch::Sender<QuiescentState>,
}

pub enum ServerStatusNotification {}

#[derive(Deserialize, Serialize, Debug)]
pub struct ServerStatusParams {
    pub quiescent: bool,
    #[serde(default)]
    pub health: String,
}

impl Notification for ServerStatusNotification {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "experimental/serverStatus";
}

pub struct Client {
    runtime: Runtime,
    server: ServerSocket,
    quiescent_rx: watch::Receiver<QuiescentState>,
    pid: Option<u32>,
    // Kept alive so kill_on_drop fires when the client is dropped.
    _child: Child,
    _mainloop: JoinHandle<()>,
    _stderr_drain: JoinHandle<()>,
}

impl Client {
    /// Spawn rust-analyzer (must be on `$PATH`).
    ///
    /// We deliberately do **not** set `current_dir` to the target workspace: doing so
    /// makes rustup's proxy resolve the toolchain via the workspace's
    /// `rust-toolchain.toml`, and if that toolchain lacks rust-analyzer the proxy
    /// dies with "Unknown binary". Workspace info travels via `rootUri` instead.
    pub fn spawn(_workspace: &Path) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;

        let mut child = {
            let _guard = runtime.enter();
            Command::new("rust-analyzer")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .context("failed to spawn rust-analyzer (is it on PATH?)")?
        };

        let pid = child.id();
        let stdin = child.stdin.take().context("rust-analyzer stdin missing")?;
        let stdout = child
            .stdout
            .take()
            .context("rust-analyzer stdout missing")?;
        let stderr = child
            .stderr
            .take()
            .context("rust-analyzer stderr missing")?;

        let (quiescent_tx, quiescent_rx) = watch::channel(QuiescentState::default());

        let (mainloop, server) = MainLoop::new_client(move |_socket| {
            let mut router = Router::new(ClientState {
                generation: 0,
                quiescent_tx,
            });
            router.notification::<ServerStatusNotification>(|state, params| {
                state.generation += 1;
                let _ = state.quiescent_tx.send(QuiescentState {
                    quiescent: params.quiescent,
                    generation: state.generation,
                });
                ControlFlow::Continue(())
            });
            router.unhandled_notification(|_, _| ControlFlow::Continue(()));

            ServiceBuilder::new()
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(router)
        });

        let mainloop_handle = runtime.spawn(async move {
            let stdout = stdout.compat();
            let stdin = stdin.compat_write();
            if let Err(e) = mainloop.run_buffered(stdout, stdin).await {
                tracing::error!("ra-mainloop error: {e:?}");
            }
        });

        let stderr_handle = runtime.spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::trace!("ra-stderr: {line}");
            }
        });

        Ok(Self {
            runtime,
            server,
            quiescent_rx,
            pid,
            _child: child,
            _mainloop: mainloop_handle,
            _stderr_drain: stderr_handle,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn wait_for_quiescent_after(&self, prev_gen: u64) -> Result<u64> {
        self.runtime
            .block_on(wait_for_quiescent_after_async(&self.quiescent_rx, prev_gen))
    }

    pub fn request<R>(&self, params: R::Params) -> Result<R::Result>
    where
        R: Request + 'static,
        R::Params: Clone + Send + 'static,
        R::Result: Send + 'static,
    {
        self.runtime.block_on(request_with_retry::<R>(
            &self.server,
            &self.quiescent_rx,
            params,
        ))
    }

    pub fn request_async<R>(&self, params: R::Params) -> PendingRequest<'_, R::Result>
    where
        R: Request + 'static,
        R::Params: Clone + Send + 'static,
        R::Result: Send + 'static,
    {
        let fut = request_with_retry::<R>(&self.server, &self.quiescent_rx, params).boxed();
        PendingRequest { fut: Some(fut) }
    }

    pub fn wait_all<T>(&self, pending: Vec<PendingRequest<'_, T>>) -> Vec<Result<T>>
    where
        T: Send + 'static,
    {
        let futs: Vec<BoxFuture<'_, Result<T>>> = pending
            .into_iter()
            .map(|mut p| p.fut.take().unwrap())
            .collect();
        self.runtime.block_on(futures::future::join_all(futs))
    }

    pub fn notify<N>(&self, params: N::Params) -> Result<()>
    where
        N: Notification,
    {
        self.server
            .notify::<N>(params)
            .map_err(|e| anyhow!("LSP notify `{}` failed: {e}", N::METHOD))
    }

    pub fn shutdown_and_exit(&self) -> Result<()> {
        self.runtime.block_on(async {
            self.server
                .request::<lsp_types::request::Shutdown>(())
                .await
                .map_err(|e| anyhow!("LSP shutdown failed: {e}"))?;
            self.server
                .notify::<lsp_types::notification::Exit>(())
                .map_err(|e| anyhow!("LSP exit failed: {e}"))?;
            Ok(())
        })
    }
}

pub struct PendingRequest<'a, T> {
    fut: Option<BoxFuture<'a, Result<T>>>,
}

async fn wait_for_quiescent_after_async(
    rx: &watch::Receiver<QuiescentState>,
    prev_gen: u64,
) -> Result<u64> {
    let mut rx = rx.clone();
    loop {
        {
            let state = rx.borrow_and_update();
            if state.quiescent && state.generation > prev_gen {
                return Ok(state.generation);
            }
        }
        rx.changed()
            .await
            .map_err(|_| anyhow!("LSP mainloop closed before quiescent"))?;
    }
}

async fn request_with_retry<R>(
    server: &ServerSocket,
    quiescent_rx: &watch::Receiver<QuiescentState>,
    params: R::Params,
) -> Result<R::Result>
where
    R: Request,
    R::Params: Clone + Send + 'static,
    R::Result: Send + 'static,
{
    loop {
        let gen_before = quiescent_rx.borrow().generation;
        match server.request::<R>(params.clone()).await {
            Ok(value) => return Ok(value),
            Err(async_lsp::Error::Response(resp)) if resp.code == ErrorCode::CONTENT_MODIFIED => {
                wait_for_quiescent_after_async(quiescent_rx, gen_before).await?;
                continue;
            }
            Err(async_lsp::Error::Response(resp)) => {
                bail!(
                    "LSP error for `{}`: code={} message={}",
                    R::METHOD,
                    resp.code.0,
                    resp.message
                );
            }
            Err(e) => bail!("LSP request `{}` failed: {e}", R::METHOD),
        }
    }
}

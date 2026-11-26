//! A user-selected executable may ask one private question when a root turn settles.
//! The program reads a completion record, writes one JSON line, and reads the answer.
//! Host selection is independent of workspace configuration and model-visible tools.
use crate::thread_sample::{SampleResponseRegistry, SampleTask};
use anyhow::{Context, Result, ensure};
use codex_app_server_protocol::ThreadSampleParams;
use codex_core::{ThreadManager, config::Config};
use codex_extension_api::{
    ExtensionFuture, ThreadIdleCause, ThreadIdleInput, ThreadLifecycleContributor,
    ThreadStartInput, ThreadStopInput, TurnLifecycleContributor, TurnStartInput,
};
use codex_protocol::{ThreadId, protocol::SessionSource};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_QUESTION_BYTES: u64 = 16 * 1024;

pub(crate) struct TurnObserver {
    executable: PathBuf,
    manager: Weak<ThreadManager>,
    samples: Arc<SampleResponseRegistry>,
    slots: Arc<Semaphore>,
}

struct Enabled;
struct PendingTurn {
    id: String,
    cancel: CancellationToken,
}
struct ActiveObservation(CancellationToken);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    question: Option<String>,
}

impl TurnObserver {
    pub(crate) fn configured(
        manager: Weak<ThreadManager>,
        samples: Arc<SampleResponseRegistry>,
    ) -> Option<Arc<Self>> {
        let executable = PathBuf::from(std::env::var_os("CODEX_TURN_OBSERVER")?);
        if !executable.is_absolute() {
            tracing::warn!("CODEX_TURN_OBSERVER requires an absolute executable path");
            return None;
        }
        Some(Arc::new(Self {
            executable,
            manager,
            samples,
            slots: Arc::new(Semaphore::new(1)),
        }))
    }

    fn cancel(store: &codex_extension_api::ExtensionData) {
        if let Some(active) = store.remove::<ActiveObservation>() {
            active.0.cancel();
        }
    }
}

impl ThreadLifecycleContributor<Config> for TurnObserver {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if !input.config.ephemeral
                && matches!(
                    input.session_source,
                    SessionSource::Cli | SessionSource::VSCode | SessionSource::Exec
                )
            {
                input.thread_store.insert(Enabled);
            }
        })
    }

    fn on_thread_idle<'a>(&'a self, input: ThreadIdleInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(turn) = input.thread_store.remove::<PendingTurn>() else {
                return;
            };
            if input.cause != ThreadIdleCause::Completed {
                return;
            }
            let Ok(permit) = self.slots.clone().try_acquire_owned() else {
                return;
            };
            let Ok(thread) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            let Some(manager) = self.manager.upgrade() else {
                return;
            };
            let path = self.executable.clone();
            let samples = self.samples.clone();
            // A new turn or teardown cancels this token even after admission.
            let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
            tokio::spawn(originator.scope(async move {
                let _permit = permit;
                if let Err(error) = observe(path, manager, samples, thread, &turn).await {
                    tracing::debug!(%error, "private turn observation stopped");
                }
            }));
        })
    }

    fn on_thread_stop<'a>(&'a self, input: ThreadStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            Self::cancel(input.thread_store);
        })
    }
}

impl TurnLifecycleContributor for TurnObserver {
    fn on_turn_start<'a>(&'a self, input: TurnStartInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if input.thread_store.get::<Enabled>().is_none() {
                return;
            }
            Self::cancel(input.thread_store);
            let cancel = CancellationToken::new();
            input.thread_store.insert(ActiveObservation(cancel.clone()));
            input.thread_store.insert(PendingTurn {
                id: input.turn_id.to_owned(),
                cancel,
            });
        })
    }
}

async fn observe(
    path: PathBuf,
    manager: Arc<ThreadManager>,
    samples: Arc<SampleResponseRegistry>,
    thread: ThreadId,
    turn: &PendingTurn,
) -> Result<()> {
    let parent = manager.get_thread(thread).await?;
    if turn.cancel.is_cancelled() {
        return Ok(());
    }
    let mut child = Command::new(path)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().context("missing callback stdin")?;
    let stdout = child.stdout.take().context("missing callback stdout")?;
    let offer = json!({"type":"completed","threadId":thread,"turnId":turn.id});
    let prepare = async {
        stdin.write_all(format!("{offer}\n").as_bytes()).await?;
        let mut bytes = Vec::new();
        BufReader::new(stdout.take(MAX_QUESTION_BYTES + 1))
            .read_until(b'\n', &mut bytes)
            .await?;
        ensure!(
            bytes.len() <= MAX_QUESTION_BYTES as usize && bytes.ends_with(b"\n"),
            "invalid callback frame"
        );
        Ok::<_, anyhow::Error>(serde_json::from_slice::<Decision>(&bytes)?.question)
    };
    let question = tokio::select! {
        _ = turn.cancel.cancelled() => return Ok(()),
        result = timeout(CALLBACK_TIMEOUT, prepare) => result??,
    };
    let Some(prompt) = question.filter(|q| !q.trim().is_empty()) else {
        return Ok(());
    };
    let task = SampleTask {
        store: manager.thread_store(),
        manager,
        parent,
        samples,
        params: ThreadSampleParams {
            thread_id: thread.to_string(),
            expected_turn_id: turn.id.clone(),
            prompt,
            interrupt_on_cancel: Some(true),
            ..Default::default()
        },
        trace: None,
        cancellation: turn.cancel.clone(),
    };
    // This await owns the entire fork, including cancellation and shutdown.
    let result = task.run_observer().await;
    let reply = match result {
        Ok(sample) => json!({"type":"sample","sample":sample}),
        Err(_) => json!({"type":"skipped"}),
    };
    tokio::select! {
        _ = turn.cancel.cancelled() => Ok(()),
        result = timeout(CALLBACK_TIMEOUT, async {
            stdin.write_all(format!("{reply}\n").as_bytes()).await?;
            drop(stdin);
            ensure!(child.wait().await?.success(), "callback delivery failed");
            Ok(())
        }) => result?,
    }
}

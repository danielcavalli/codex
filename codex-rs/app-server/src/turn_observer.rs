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
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::{ThreadId, protocol::SessionSource};
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
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
    budget: Arc<Mutex<Budget>>,
}

/// Admission is charged before the fork, including requests lost to cancellation.
#[derive(Default)]
struct Budget {
    attempts: VecDeque<Instant>,
    cold: u8,
    pause_until: Option<Instant>,
}

impl Budget {
    fn admit(&mut self, now: Instant) -> bool {
        self.attempts
            .retain(|time| now.duration_since(*time) < Duration::from_secs(3600));
        if self.attempts.len() >= 12 || self.pause_until.is_some_and(|end| now < end) {
            return false;
        }
        self.attempts.push_back(now);
        true
    }

    fn observe_cache(&mut self, cached: Option<i64>) {
        if cached.is_some_and(|tokens| tokens > 0) {
            self.cold = 0;
        } else {
            self.cold += 1;
            if self.cold >= 2 {
                self.pause_until = Some(Instant::now() + Duration::from_secs(600));
                self.cold = 0;
            }
        }
    }
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
            budget: Default::default(),
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
            let budget = self.budget.clone();
            // A new turn or teardown cancels this token even after admission.
            let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
            tokio::spawn(originator.scope(async move {
                let _permit = permit;
                if let Err(error) = observe(path, manager, samples, budget, thread, &turn).await {
                    tracing::warn!(%error, "private turn observation stopped");
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

/// The completion record the observer reads first.
fn offer(thread: ThreadId, turn: &str, cwd: &str) -> serde_json::Value {
    json!({"type": "completed", "threadId": thread, "turnId": turn, "cwd": cwd})
}

async fn observe(
    path: PathBuf,
    manager: Arc<ThreadManager>,
    samples: Arc<SampleResponseRegistry>,
    budget: Arc<Mutex<Budget>>,
    thread: ThreadId,
    turn: &PendingTurn,
) -> Result<()> {
    let parent = manager.get_thread(thread).await?;
    if turn.cancel.is_cancelled() {
        return Ok(());
    }
    // The observer starts in the temp directory; the thread's own directory travels in the offer
    // so the observer can read its project's settings.
    let cwd = parent.config_snapshot().await.cwd().display().to_string();
    let mut child = Command::new(path)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().context("missing callback stdin")?;
    let stdout = child.stdout.take().context("missing callback stdout")?;
    let offer = offer(thread, &turn.id, &cwd);
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
        // EOF acknowledges the skip. Allow the callback to close its store normally.
        drop(stdin);
        tokio::select! {
            _ = turn.cancel.cancelled() => {},
            result = timeout(CALLBACK_TIMEOUT, child.wait()) => { result??; },
        }
        return Ok(());
    };
    if !budget
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .admit(Instant::now())
    {
        return Ok(());
    }
    let task = SampleTask {
        store: manager.thread_store(),
        manager,
        parent,
        samples,
        params: ThreadSampleParams {
            thread_id: thread.to_string(),
            expected_turn_id: turn.id.clone(),
            prompt,
            // Reasoning counts against the private answer's output cap; a side question needs
            // little of it whatever the session runs at.
            effort: Some(ReasoningEffort::Low),
            interrupt_on_cancel: Some(true),
            ..Default::default()
        },
        trace: None,
        cancellation: turn.cancel.clone(),
    };
    // This await owns the entire fork, including cancellation and shutdown.
    let result = task.run_observer().await;
    let reply = match result {
        Ok(sample) => {
            budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .observe_cache(sample.cached_input_tokens);
            json!({"type":"sample","sample":sample})
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_admits_twelve_attempts_an_hour() {
        let mut budget = Budget::default();
        let start = Instant::now();
        for minute in 0..12 {
            assert!(budget.admit(start + Duration::from_secs(minute * 60)));
        }
        assert!(!budget.admit(start + Duration::from_secs(12 * 60)));
        // The first attempt leaves the window an hour after it was made.
        assert!(budget.admit(start + Duration::from_secs(3600)));
    }

    #[test]
    fn two_cold_answers_pause_admission_and_a_warm_one_resets_the_count() {
        let mut budget = Budget::default();
        budget.observe_cache(Some(0));
        budget.observe_cache(Some(17_536));
        budget.observe_cache(None);
        assert!(budget.pause_until.is_none(), "a warm answer between cold ones resets them");
        budget.observe_cache(Some(0));
        let paused = budget.pause_until.expect("two cold answers in a row pause");
        assert!(!budget.admit(Instant::now()));
        assert!(budget.admit(paused + Duration::from_secs(1)));
    }

    #[test]
    fn the_offer_names_the_turn_and_the_threads_directory() {
        let thread = ThreadId::new();
        let record = offer(thread, "turn-1", "/home/dan/Work/dan-cli");
        assert_eq!(record["type"], "completed");
        assert_eq!(record["threadId"], json!(thread));
        assert_eq!(record["turnId"], "turn-1");
        assert_eq!(record["cwd"], "/home/dan/Work/dan-cli");
    }
}

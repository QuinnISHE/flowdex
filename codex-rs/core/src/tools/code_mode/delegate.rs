use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use codex_code_mode::CellId;
use codex_code_mode::CodeModeNestedToolCall;
use codex_code_mode::CodeModeSessionDelegate;
use codex_code_mode::NotificationFuture;
use codex_code_mode::ToolInvocationFuture;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use serde_json::Value as JsonValue;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::ExecContext;
use super::PUBLIC_TOOL_NAME;
use super::call_nested_tool;
use crate::session::step_context::StepContext;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;

pub(super) struct CodeModeDispatchBroker {
    dispatch_gates: Arc<Mutex<HashMap<CellId, watch::Sender<bool>>>>,
    active_host: Arc<Mutex<Option<(u64, Arc<CoreTurnHost>)>>>,
    cell_hosts: Arc<Mutex<HashMap<CellId, Arc<CoreTurnHost>>>>,
    next_host_id: AtomicU64,
}

impl CodeModeDispatchBroker {
    pub(super) fn new() -> Self {
        Self {
            dispatch_gates: Arc::new(Mutex::new(HashMap::new())),
            active_host: Arc::new(Mutex::new(None)),
            cell_hosts: Arc::new(Mutex::new(HashMap::new())),
            next_host_id: AtomicU64::new(0),
        }
    }

    pub(super) fn mark_cell_ready_for_dispatch(
        &self,
        cell_id: &CellId,
        parent_cell_id: Option<&CellId>,
    ) -> Result<(), String> {
        let host = if let Some(parent_cell_id) = parent_cell_id {
            lock_or_recover(&self.cell_hosts)
                .get(parent_cell_id)
                .map(|host| Arc::new(host.fork_dispatch_context()))
                .ok_or_else(|| {
                    format!("code mode parent cell {parent_cell_id} has no dispatch host")
                })?
        } else {
            lock_or_recover(&self.active_host)
                .as_ref()
                .map(|(_, host)| Arc::clone(host))
                .ok_or_else(|| "code mode turn has no dispatch host".to_string())?
        };
        lock_or_recover(&self.cell_hosts).insert(cell_id.clone(), host);
        dispatch_gate(&self.dispatch_gates, cell_id).send_replace(true);
        Ok(())
    }

    pub(super) fn close_cell(&self, cell_id: &CellId) {
        remove_dispatch_gate(&self.dispatch_gates, cell_id);
        lock_or_recover(&self.cell_hosts).remove(cell_id);
    }

    pub(super) fn start_turn_worker(
        &self,
        exec: ExecContext,
        router: Arc<ToolRouter>,
        step_context: Arc<StepContext>,
        tracker: SharedTurnDiffTracker,
    ) -> CodeModeDispatchWorker {
        let tool_runtime =
            ToolCallRuntime::new(router, Arc::clone(&exec.session), step_context, tracker);
        let host = Arc::new(CoreTurnHost { exec, tool_runtime });
        let host_id = self.next_host_id.fetch_add(1, Ordering::Relaxed);
        *lock_or_recover(&self.active_host) = Some((host_id, host));
        CodeModeDispatchWorker {
            active_host: Arc::clone(&self.active_host),
            host_id,
        }
    }

    fn cell_host(&self, cell_id: &CellId) -> Result<Arc<CoreTurnHost>, String> {
        lock_or_recover(&self.cell_hosts)
            .get(cell_id)
            .cloned()
            .ok_or_else(|| format!("code mode cell {cell_id} has no dispatch host"))
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn dispatch_gate(
    dispatch_gates: &Mutex<HashMap<CellId, watch::Sender<bool>>>,
    cell_id: &CellId,
) -> watch::Sender<bool> {
    let mut dispatch_gates = match dispatch_gates.lock() {
        Ok(dispatch_gates) => dispatch_gates,
        Err(poisoned) => poisoned.into_inner(),
    };
    dispatch_gates
        .entry(cell_id.clone())
        .or_insert_with(|| watch::channel(false).0)
        .clone()
}

fn remove_dispatch_gate(
    dispatch_gates: &Mutex<HashMap<CellId, watch::Sender<bool>>>,
    cell_id: &CellId,
) {
    let mut dispatch_gates = match dispatch_gates.lock() {
        Ok(dispatch_gates) => dispatch_gates,
        Err(poisoned) => poisoned.into_inner(),
    };
    dispatch_gates.remove(cell_id);
}

async fn wait_until_cell_ready_for_dispatch(
    dispatch_gates: &Mutex<HashMap<CellId, watch::Sender<bool>>>,
    cell_id: &CellId,
    cancellation_token: &CancellationToken,
) -> bool {
    if cancellation_token.is_cancelled() {
        return false;
    }
    let mut ready_rx = dispatch_gate(dispatch_gates, cell_id).subscribe();
    loop {
        if *ready_rx.borrow_and_update() {
            return true;
        }
        tokio::select! {
            changed = ready_rx.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            _ = cancellation_token.cancelled() => return false,
        }
    }
}

impl CodeModeSessionDelegate for CodeModeDispatchBroker {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                return Err("code mode nested tool call cancelled".to_string());
            }
            let cell_id = invocation.cell_id.clone();
            if !wait_until_cell_ready_for_dispatch(
                &self.dispatch_gates,
                &cell_id,
                &cancellation_token,
            )
            .await
            {
                self.close_cell(&cell_id);
                return Err("code mode nested tool call cancelled".to_string());
            }
            let host = self.cell_host(&cell_id)?;
            tokio::select! {
                response = host.invoke_tool(invocation, cancellation_token.clone()) => response,
                _ = cancellation_token.cancelled() => Err("code mode nested tool call cancelled".to_string()),
            }
        })
    }

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                return Err("code mode notification cancelled".to_string());
            }
            if !wait_until_cell_ready_for_dispatch(
                &self.dispatch_gates,
                &cell_id,
                &cancellation_token,
            )
            .await
            {
                self.close_cell(&cell_id);
                return Err("code mode notification cancelled".to_string());
            }
            let host = self.cell_host(&cell_id)?;
            tokio::select! {
                response = host.notify(call_id, cell_id, text) => response,
                _ = cancellation_token.cancelled() => Err("code mode notification cancelled".to_string()),
            }
        })
    }

    fn cell_closed(&self, cell_id: &CellId) {
        self.close_cell(cell_id);
    }
}

pub(crate) struct CodeModeDispatchWorker {
    active_host: Arc<Mutex<Option<(u64, Arc<CoreTurnHost>)>>>,
    host_id: u64,
}

impl Drop for CodeModeDispatchWorker {
    fn drop(&mut self) {
        let mut active_host = lock_or_recover(&self.active_host);
        if active_host
            .as_ref()
            .is_some_and(|(host_id, _)| *host_id == self.host_id)
        {
            *active_host = None;
        }
    }
}

struct CoreTurnHost {
    exec: ExecContext,
    tool_runtime: ToolCallRuntime,
}

impl CoreTurnHost {
    fn fork_dispatch_context(&self) -> Self {
        Self {
            exec: self.exec.clone(),
            tool_runtime: self.tool_runtime.fork_dispatch_context(),
        }
    }

    async fn invoke_tool(
        &self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> Result<JsonValue, String> {
        call_nested_tool(
            self.exec.clone(),
            self.tool_runtime.clone(),
            invocation,
            cancellation_token,
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn notify(&self, call_id: String, cell_id: CellId, text: String) -> Result<(), String> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.exec
            .session
            .inject_if_running(vec![ResponseItem::CustomToolCallOutput {
                id: None,
                call_id,
                name: Some(PUBLIC_TOOL_NAME.to_string()),
                output: FunctionCallOutputPayload::from_text(text),
                internal_chat_message_metadata_passthrough: None,
            }])
            .await
            .map_err(|_| {
                format!("failed to inject exec notify message for cell {cell_id}: no active turn")
            })
    }
}

use super::context_window::context_window_token_status;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::context::CompactionReminder;
use crate::context::ContextualUserFragment;
use codex_protocol::error::Result as CodexResult;

#[derive(Default)]
pub(crate) struct CompactionReminderState {
    last_recorded_window_id: Option<String>,
}

impl CompactionReminderState {
    pub(crate) fn due(&self, window_id: &str, active_context_tokens: i64, threshold: i64) -> bool {
        active_context_tokens >= threshold
            && self.last_recorded_window_id.as_deref() != Some(window_id)
    }

    pub(crate) fn record(&mut self, window_id: &str) {
        self.last_recorded_window_id = Some(window_id.to_string());
    }
}

pub(crate) async fn maybe_record(
    sess: &Session,
    turn_context: &TurnContext,
    window_id: &str,
) -> CodexResult<()> {
    let threshold = turn_context
        .config
        .flowdex_config
        .compaction_reminder_threshold_tokens;
    let active_context_tokens = context_window_token_status(sess, turn_context)
        .await
        .active_context_tokens;
    let due = {
        let state = sess.state.lock().await;
        state
            .compaction_reminder
            .due(window_id, active_context_tokens, threshold)
    };
    if !due {
        return Ok(());
    }

    let response_item = ContextualUserFragment::into(CompactionReminder);
    sess.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
        .await;
    sess.state
        .lock()
        .await
        .compaction_reminder
        .record(window_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CompactionReminderState;

    #[test]
    fn is_due_once_per_window_at_threshold() {
        let mut state = CompactionReminderState::default();
        assert!(!state.due("window-a", 99, 100));
        assert!(state.due("window-a", 100, 100));
        state.record("window-a");
        assert!(!state.due("window-a", 101, 100));
        assert!(state.due("window-b", 100, 100));
    }
}

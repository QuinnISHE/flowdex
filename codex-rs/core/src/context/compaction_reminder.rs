use super::ContextualUserFragment;

pub(crate) struct CompactionReminder;

impl ContextualUserFragment for CompactionReminder {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        "Your context window is growing. At the next natural task boundary, call compact_context."
            .to_string()
    }
}

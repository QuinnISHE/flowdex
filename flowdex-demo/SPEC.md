# Incident digest contract

Parse one incident per line using `severity|title|owner`.

- Severity is case-insensitive: `low`, `medium`, `high`, or `critical`.
- Trim each field and reject missing or extra fields.
- Reject an empty title or owner and name the invalid field in the error.
- Reject an unknown severity with an error containing `unknown severity`.

Render a Markdown digest with this exact shape:

```text
# Incident digest

- [CRITICAL] Database unavailable (@on-call)
- [LOW] Update dashboard (@maya)
```

Sort by severity from critical to low. Preserve input order within one severity.

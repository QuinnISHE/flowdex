---
name: report-flowdex-review
description: Report precise, attributable findings from an active Flowdex task or phase review. Use when a Flowdex reviewer must pass or submit line-based defects for automatic routing, repair, resolution tracking, and optional AST-grep rule candidacy.
---

# Report Flowdex Review

Inspect the requirements, current diff, and relevant code. Submit one report after the review is complete.

## Report findings

```text
report_flowdex_review({
  findings: [{
    file: "src/parser.rs",
    lineStart: 84,
    lineEnd: 89,
    reason: "The length is read as little-endian, but the format requires big-endian.",
    ruleKey: "parser-length-endianness",
    astGrepSuitable: false
  }]
})
```

For each finding:

- Point to the smallest current line range that proves the defect.
- State the broken behavior and required correction.
- Report only actionable defects caused or exposed by the reviewed change.
- Set `astGrepSuitable: true` only for a repeatable syntax-shaped mistake that a native AST-grep rule could detect. Then provide a stable, non-blank `ruleKey`.

Do not guess attribution or choose a repair agent. Flowdex maps reviewed lines through integrated and source commits to the responsible task operation. Unattributed or exhausted findings suspend to the configured boundary.

## Pass

If there are no actionable findings, submit:

```text
report_flowdex_review({ findings: [] })
```

Do not send a separate prose verdict or message the worker directly. The durable report is the review result; the scheduler routes findings and records exact repair resolutions.

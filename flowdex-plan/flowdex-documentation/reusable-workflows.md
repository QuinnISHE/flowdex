# Reusable and nested workflows

Saved workflows can be reusable components. A child declares and validates
its input, then returns one JSON-compatible value:

```js
const input = flowdex.requireInput({
  properties: {
    files: { type: "array", items: { type: "string" } },
    strict: { type: "boolean" },
  },
  required: ["files"],
});

const checked = await checkFiles(input.files, input.strict ?? false);
flowdex.output({ checked });
```

The parent may conditionally invoke that child with an explicit reference:

```js
const input = flowdex.input ?? {};
const documentation = input.check
  ? await flowdex.runWorkflow(
      "global:documentation/check",
      { files: input.files, strict: input.strict ?? false },
    )
  : null;

flowdex.output({ documentation });
```

`repo:name/or/path` resolves to
`.flowdex/workflows/name/or/path.js` beneath the trusted repository. A
`global:name/or/path` reference resolves to
`$CODEX_HOME/flowdex/workflows/name/or/path.js`. References must use one
explicit scope and normalized, non-empty path segments; absolute paths,
traversal, alternate separators, embedded extensions, and other path syntax
are rejected. Repository files never shadow global files.

The model saves a reusable workflow, then starts it using the returned
reference:

```text
save_flowdex_workflow({
  workflow: "global:documentation/check",
  source: "<the saved JavaScript source>"
})
start_flowdex_workflow({
  path: "global:documentation/check",
  input: { files: ["README.md"], strict: true }
})
```

Saving is direct-model-only and writes only the named repository or global
workflow location. Starting remains the direct model tool; nested workflows
use `runWorkflow`, not `start_flowdex_workflow`. Omitted input is `{}` for
validation, and child output is the exact JSON value returned to the parent.
If a child does not call `flowdex.output`, its result is `null`; invalid or
ambiguous non-JSON output is an error.

Nesting is event-driven: the parent awaits the child without an intermediate
model turn. Child scheduler progress remains visible as automatic live app
progress, and workflow-spawned agents continue to emit their normal
app-visible lifecycle and graph events. These events are not inserted into
the parent model's history or child result.

const input = flowdex.requireInput({
  properties: {
    request: { type: "string" },
  },
  required: ["request"],
});

const research = await flowdex.runWorkflow("repo:defaults/research-rounds", {
  topic: "the flowdex-demo incident digest contract, tests, and existing source",
});
await flowdex.signal("research-complete");

const run = await flowdex.startRun({
  name: "incident-digest-demo",
  agents: {
    explorer: { profile: "explorer" },
    worker: { profile: "implementation_worker_fast" },
    reviewer: { model: "gpt-5.6-luna", reasoningEffort: "xhigh" },
  },
  contextPacks: {
    "incident-contract": {
      agent: "explorer",
      instructions: "Use $collect-flowdex-context to read flowdex-demo/SPEC.md and flowdex-demo/tests/digest.rs, then publish focused parser and renderer fragments to incident-contract.",
    },
  },
  phases: [
    {
      name: "implementation",
      instructions: `Implement ${input.request}. Commit each modification with a brief summary. Independent tasks should not edit each other's files. Research results: ${JSON.stringify(research)}`,
      open: true,
      boundary: "human",
      verification: ["cargo test --manifest-path flowdex-demo/Cargo.toml"],
      review: {
        agent: "reviewer",
        instructions: "Use $report-flowdex-review to review the integrated incident digest against SPEC.md and the tests. Report only concrete defects.",
        maxRounds: 2,
      },
      tasks: [
        {
          name: "parse-input",
          agent: "worker",
          instructions: "Implement parser.rs. Keep the public API unchanged.",
          context: ["incident-contract"],
          readScope: ["flowdex-demo/SPEC.md", "flowdex-demo/tests/digest.rs"],
          writeScope: ["flowdex-demo/src/parser.rs"],
          verification: [
            "cargo test --manifest-path flowdex-demo/Cargo.toml parse_",
          ],
          verificationRepairLimit: 1,
        },
        {
          name: "render-digest",
          agent: "worker",
          instructions: "Implement render.rs. Keep the public API unchanged.",
          context: ["incident-contract"],
          readScope: ["flowdex-demo/SPEC.md", "flowdex-demo/tests/digest.rs"],
          writeScope: ["flowdex-demo/src/render.rs"],
          verification: [
            "cargo test --manifest-path flowdex-demo/Cargo.toml render_",
          ],
          verificationRepairLimit: 1,
        },
      ],
    },
  ],
});

await run.queueTask("implementation", {
  name: "document-result",
  agent: "worker",
  instructions: "Replace the starter README with a concise usage example matching SPEC.md.",
  dependencies: ["parse-input", "render-digest"],
  readScope: ["flowdex-demo/SPEC.md"],
  writeScope: ["flowdex-demo/README.md"],
});
await run.sealPhase("implementation");

flowdex.output(await run.wait());

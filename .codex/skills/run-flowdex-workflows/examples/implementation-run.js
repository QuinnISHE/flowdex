const input = flowdex.requireInput({
  properties: {
    request: { type: "string" },
    verification: { type: "array", items: { type: "string" } },
  },
  required: ["request", "verification"],
});

const run = await flowdex.startRun({
  name: "implementation",
  agents: {
    explorer: { model: "gpt-5.6-luna", reasoningEffort: "high" },
    worker: { model: "gpt-5.6-sol", reasoningEffort: "high" },
    reviewer: { model: "gpt-5.6-luna", reasoningEffort: "xhigh" },
  },
  contextPacks: {
    architecture: {
      agent: "explorer",
      instructions: "Publish the relevant entry points, invariants, and tests as small source ranges with stable keys.",
    },
  },
  phases: [{
    name: "build",
    instructions: `Implement this request while preserving existing behavior: ${input.request}`,
    boundary: "orchestrator",
    verification: ["git diff --check"],
    review: {
      agent: "reviewer",
      instructions: "Review the integrated phase for correctness, regressions, and missing error handling.",
      maxRounds: 2,
    },
    tasks: [{
      name: "implementation",
      agent: "worker",
      instructions: "Implement the production change and keep the diff focused.",
      readScope: ["src/**", "tests/**"],
      writeScope: ["src/**"],
      context: ["architecture"],
      verification: input.verification,
      verificationRepairLimit: 1,
    }, {
      name: "coverage",
      agent: "worker",
      instructions: "Add focused regression coverage for the requested behavior.",
      readScope: ["src/**", "tests/**"],
      writeScope: ["tests/**"],
      context: ["architecture"],
    }],
  }],
});

flowdex.output(await run.wait());

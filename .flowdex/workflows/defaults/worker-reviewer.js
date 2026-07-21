const input = flowdex.requireInput({
  properties: {
    name: { type: "string" },
    instructions: { type: "string" },
    verification: { type: "array", items: { type: "string" } },
    writeScope: { type: "array", items: { type: "string" } },
  },
  required: ["name", "instructions", "verification", "writeScope"],
});

const run = await flowdex.startRun({
  name: input.name,
  agents: {
    worker: { model: "gpt-5.6-sol", reasoningEffort: "high" },
    reviewer: { model: "gpt-5.6-luna", reasoningEffort: "xhigh" },
  },
  phases: [
    {
      name: "implementation",
      instructions: "Implement the request and finish with a brief summary.",
      tasks: [
        {
          name: "implement",
          agent: "worker",
          instructions: input.instructions,
          writeScope: input.writeScope,
          verification: input.verification,
          verificationRepairLimit: 1,
          review: {
            agent: "reviewer",
            instructions: "Use $report-flowdex-review to review the requirements and committed diff. Report only concrete defects.",
            maxRounds: 2,
          },
        },
      ],
    },
  ],
});

flowdex.output(await run.wait());

const input = flowdex.requireInput({
  properties: {
    label: { type: "string" },
  },
  required: ["label"],
});

const messaging = await flowdex.runWorkflow("repo:flowdex-test/messaging-rounds", {
  topic: `the purpose of the Flowdex validation fixture named ${input.label}`,
});
if (messaging.lunaStatus !== "completed" || messaging.solStatus !== "completed") {
  throw new Error("nested messaging workflow did not complete both agents");
}

const astRules = await flowdex.checkRules(["flowdex-test-no-console"]);
const expectedFinding = astRules.findings.some(
  (finding) =>
    finding.ruleId === "flowdex-test-no-console" &&
    finding.file === "flowdex-test/fixtures/ast-target.js",
);
if (astRules.passed || !expectedFinding) {
  throw new Error("AST-grep did not report the intentional validation finding");
}

await flowdex.signal("preflight_complete");

const run = await flowdex.startRun({
  name: `flowdex-validation-${input.label}`,
  agents: {
    explorer: { profile: "explorer" },
    worker_a: { profile: "implementation_worker_fast" },
    worker_b: { profile: "implementation_worker_fast" },
    reviewer: {
      model: "gpt-5.6-luna",
      reasoningEffort: "xhigh",
    },
  },
  contextPacks: {
    fixture_contract: {
      agent: "explorer",
      instructions:
        "Use $collect-flowdex-context. Publish pack fixture_contract, key required_token, path flowdex-test/context/contract.md, inclusive lines 1 through 5, with a short summary. Do not edit files.",
    },
  },
  phases: [
    {
      name: "validation",
      instructions:
        "This is a disposable Flowdex validation. Modify only flowdex-test/results. Commit every modification with a brief summary.",
      open: true,
      verification: [
        "git grep -Fq \"context token: cobalt\" -- flowdex-test/results/context-result.md",
        "git grep -Fq \"parallel branch: complete\" -- flowdex-test/results/parallel-result.md",
        "git grep -Fq \"answer: blue\" -- flowdex-test/results/review-target.md",
        "git grep -Fq \"dynamic task: complete\" -- flowdex-test/results/dynamic-result.md",
      ],
      tasks: [
        {
          name: "use-context",
          agent: "worker_a",
          instructions:
            "Use the automatically injected fixture_contract fragment. Create flowdex-test/results/context-result.md containing exactly one line: context token: cobalt. Commit it.",
          context: ["fixture_contract"],
          readScope: ["flowdex-test/context/contract.md"],
          writeScope: ["flowdex-test/results/context-result.md"],
          verification: ["git diff --check"],
          verificationRepairLimit: 1,
        },
        {
          name: "parallel-note",
          agent: "worker_b",
          instructions:
            "Create flowdex-test/results/parallel-result.md containing exactly one line: parallel branch: complete. Commit it.",
          writeScope: ["flowdex-test/results/parallel-result.md"],
          verification: ["git diff --check"],
          verificationRepairLimit: 1,
        },
        {
          name: "review-repair",
          agent: "worker_a",
          instructions:
            "The required final value is answer: blue. For the first implementation, deliberately create flowdex-test/results/review-target.md with exactly one incorrect line: answer: red, then commit it. When the reviewer reports that defect, replace red with blue and commit the repair.",
          writeScope: ["flowdex-test/results/review-target.md"],
          verification: ["git diff --check"],
          verificationRepairLimit: 1,
          review: {
            agent: "reviewer",
            instructions:
              "Use $report-flowdex-review. The required final content of flowdex-test/results/review-target.md is exactly `answer: blue`. If it says red, report that concrete defect at line 1 with astGrepSuitable false. After repair, report an empty findings array.",
            maxRounds: 2,
          },
        },
      ],
    },
  ],
});

const queued = await run.queueTask("validation", {
  name: "dynamic-finish",
  agent: "worker_b",
  instructions:
    "After the declared tasks finish, create flowdex-test/results/dynamic-result.md containing exactly one line: dynamic task: complete. Commit it.",
  dependencies: ["use-context", "parallel-note", "review-repair"],
  writeScope: ["flowdex-test/results/dynamic-result.md"],
  verification: ["git diff --check"],
  verificationRepairLimit: 1,
});
await run.sealPhase("validation");

const result = await run.wait();
flowdex.output({
  result,
  queuedTaskId: queued.taskId,
  messaging: {
    lunaStatus: messaging.lunaStatus,
    solStatus: messaging.solStatus,
  },
  astRule: {
    passed: astRules.passed,
    findings: astRules.findings.length,
    expectedFinding,
  },
});

const input = flowdex.requireInput({
  properties: {
    topic: { type: "string" },
  },
  required: ["topic"],
});

const first = await flowdex.spawnAgent({
  name: "research_a",
  instructions: `Research ${input.topic}. Return concrete findings with file and line references.`,
  model: "gpt-5.6-luna",
  reasoningEffort: "high",
});
const second = await flowdex.spawnAgent({
  name: "research_b",
  instructions: `Independently research ${input.topic}. Look for gaps and conflicting evidence.`,
  model: "gpt-5.6-luna",
  reasoningEffort: "high",
});

let firstResult = await flowdex.waitAgent(first);
let secondResult = await flowdex.waitAgent(second);

for (let round = 0; round < 2; round++) {
  if (firstResult.status !== "completed" || secondResult.status !== "completed") break;

  await flowdex.sendMessage(
    first,
    `Challenge or refine this report:\n${secondResult.message ?? "No report was returned."}`,
  );
  firstResult = await flowdex.resumeAgent(first, "Respond to the queued peer report.", {
    contextMode: "keep",
  });
  if (firstResult.status !== "completed") break;

  await flowdex.sendMessage(
    second,
    `Reconcile your findings with this response:\n${firstResult.message ?? "No response was returned."}`,
  );
  secondResult = await flowdex.resumeAgent(second, "Respond to the queued peer report.", {
    contextMode: "keep",
  });
}

flowdex.output({ first: firstResult, second: secondResult });

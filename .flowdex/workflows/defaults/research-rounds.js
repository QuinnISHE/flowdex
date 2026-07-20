const input = flowdex.requireInput({
  properties: {
    topic: { type: "string" },
  },
  required: ["topic"],
});

const first = await flowdex.spawnAgent({
  name: "primary_researcher",
  instructions: `Research ${input.topic}. Return concrete findings with file and line references.`,
  profile: "explorer",
});
const second = await flowdex.spawnAgent({
  name: "challenger",
  instructions: `Independently research ${input.topic}. Look for gaps and conflicting evidence.`,
  profile: "explorer",
});

let firstResult = await flowdex.waitAgent(first);
let secondResult = await flowdex.waitAgent(second);

for (let round = 0; round < 2; round++) {
  if (firstResult.status !== "completed" || secondResult.status !== "completed") break;

  await flowdex.sendMessage(
    first,
    `Challenge or refine this report:\n${secondResult.message ?? "No report was returned."}`,
    { delivery: "turn" },
  );
  firstResult = await flowdex.waitAgent(first);
  if (firstResult.status !== "completed") break;

  await flowdex.sendMessage(
    second,
    `Reconcile your findings with this response:\n${firstResult.message ?? "No response was returned."}`,
    { delivery: "turn" },
  );
  secondResult = await flowdex.waitAgent(second);
}

flowdex.output({ first: firstResult, second: secondResult });

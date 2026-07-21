const input = flowdex.requireInput({
  properties: {
    question: { type: "string" },
    maxRounds: { type: "integer" },
  },
  required: ["question", "maxRounds"],
});

const first = await flowdex.spawnAgent({
  name: "research_a",
  instructions: `Investigate independently: ${input.question}`,
  model: "gpt-5.6-luna",
  reasoningEffort: "high",
});
const second = await flowdex.spawnAgent({
  name: "research_b",
  instructions: `Investigate a different angle: ${input.question}`,
  model: "gpt-5.6-luna",
  reasoningEffort: "high",
});

let firstResult = await flowdex.waitAgent(first);
let secondResult = await flowdex.waitAgent(second);
for (let round = 0; round < input.maxRounds; round += 1) {
  if (firstResult.status !== "completed" || secondResult.status !== "completed") break;
  await flowdex.sendMessage(first, JSON.stringify(secondResult));
  firstResult = await flowdex.resumeAgent(first, "Respond to the queued peer report.", {
    contextMode: "keep",
  });
  await flowdex.sendMessage(second, JSON.stringify(firstResult));
  secondResult = await flowdex.resumeAgent(second, "Respond to the queued peer report.", {
    contextMode: "keep",
  });
}

flowdex.output({ first: firstResult, second: secondResult });

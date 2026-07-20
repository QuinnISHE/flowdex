const input = flowdex.requireInput({
  properties: {
    topic: { type: "string" },
  },
  required: ["topic"],
});

const luna = await flowdex.spawnAgent({
  name: "luna_researcher",
  instructions: `Summarize ${input.topic} in one short paragraph. End with marker LUNA_INITIAL.`,
  model: "gpt-5.6-luna",
  reasoningEffort: "medium",
});
const sol = await flowdex.spawnAgent({
  name: "sol_researcher",
  instructions: `Independently summarize ${input.topic} in one short paragraph. End with marker SOL_INITIAL.`,
  model: "gpt-5.6-sol",
  reasoningEffort: "low",
});

let lunaResult = await flowdex.waitAgent(luna);
let solResult = await flowdex.waitAgent(sol);
if (lunaResult.status !== "completed" || solResult.status !== "completed") {
  throw new Error(`initial messaging agents did not complete: ${lunaResult.status}/${solResult.status}`);
}

await flowdex.sendMessage(
  luna,
  `Read the other researcher's response, identify one agreement, and end with LUNA_REPLIED:\n${solResult.message ?? "No response"}`,
  { delivery: "turn" },
);
lunaResult = await flowdex.waitAgent(luna);
if (lunaResult.status !== "completed") {
  throw new Error(`Luna reply did not complete: ${lunaResult.status}`);
}

await flowdex.sendMessage(
  sol,
  `Read the other researcher's reply, state one conclusion, and end with SOL_REPLIED:\n${lunaResult.message ?? "No response"}`,
  { delivery: "turn" },
);
solResult = await flowdex.waitAgent(sol);
if (solResult.status !== "completed") {
  throw new Error(`Sol reply did not complete: ${solResult.status}`);
}

flowdex.output({
  lunaStatus: lunaResult.status,
  solStatus: solResult.status,
  lunaMessage: lunaResult.message,
  solMessage: solResult.message,
});

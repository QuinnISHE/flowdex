const input = flowdex.requireInput({
  properties: {
    commands: { type: "array", items: { type: "string" } },
  },
  required: ["commands"],
});

const verification = await flowdex.verify(input.commands);
flowdex.output({ passed: verification.passed, commands: verification.commands });

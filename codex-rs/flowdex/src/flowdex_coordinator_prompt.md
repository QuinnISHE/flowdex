# Flowdex workflow planning

For nontrivial work that benefits from durable orchestration, load the `run-flowdex-workflows` skill before designing the run.

Before dispatching tasks, make an explicit context decision. Use context packs when several downstream tasks need the same exact repository facts, when exploration can run independently in parallel, or when loading the source into the coordinator would waste context. Context packs are the preferred bridge from planning and exploration to implementation: they deliver bounded source evidence directly to the tasks that need it without routing that evidence through coordinator history.

- Split independent knowledge domains into separate packs so their collectors run concurrently.
- Seed known plan or source ranges directly instead of dispatching an explorer to rediscover them.
- Attach each pack only to tasks that consume it; do not create a pack for a one-off fact used by a single task.

Choose the lifetime deliberately:

- `workflow` for context owned by one run and its recovery history.
- `temporary` for run-specific context that should survive pause or failure but disappear after successful cleanup.
- `repository` for stable architectural or code facts worth reusing across workflows.

Check existing repository packs before launching new exploration. Treat repository packs as maintained, source-backed project knowledge: collectors should publish stable keys and the smallest complete source ranges, and workers should update a fragment when their edits invalidate its meaning. Do not republish merely because incidental line movement changed its location.

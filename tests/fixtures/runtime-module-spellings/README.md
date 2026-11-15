# runtime-module-spellings

One service (`runner`, the repo root) imports the same runtime module under two
spellings: `src/run.ts` from `"child_process"`, `src/watch.ts` from
`"node:child_process"`. The store states the module's exports once per spelling
the service imports it by, so the store answer holds two `ChildProcess` entries
with one store key (`node:child_process`) and two specifiers.

Each file spawns a process and listens on it with a custom event name:

- `src/run.ts:5` `proc.on("job-started", …)`
- `src/watch.ts:5` `child.on("job-finished", …)`

`spawn` returns `ChildProcessWithoutNullStreams`, an interface that extends
`ChildProcess` and declares no member of its own. The receiver's type is not
named like the labelled export, so the call reaches its label only through the
member's declaration (`on`, declared in `ChildProcess`), which the declaration
ask verifies on the instance `new ChildProcess()` builds. A receiver typed
`ChildProcess` itself would reach the label by its name and would not test the
ask.

## Answer key

The declaration ask holds one maker check (`make`, receiver `export`) for each
spelling's `ChildProcess` entry, and the op check on `instance:new` under the
`node:child_process` specifier verifies with the declaration in
`node_modules/@types/node/child_process.d.ts`.

- `src/run.ts:5` and `src/watch.ts:5` are each a possible call: kind `pubsub`,
  role `consumer`, unread reason `unanswered` (the offline scan answers no site
  question).
- Each line is an unread site: reason `unanswered`, slot `kind`, labelled end
  `ChildProcess.on`.
- No unread site at either line has slot `callee`.

The `"child_process"` spelling's own checks come back `unchecked`
(`module_local`). That is a separate verifier verdict and is not what this
fixture tests.

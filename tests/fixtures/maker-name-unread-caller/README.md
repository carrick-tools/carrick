# maker-name-unread-caller

One root service `jobs`: a queue library whose `FlowProducer`/`Queue`-style
maker binds a name that a caller fills (carrick#4520). The store answer is
`library-claims.json` (same layout as `queue-enum-base-class-one`).

A maker-bound name filled with a value no walk reads is a producer lead with
the reason the resolver meets reading it through callers; no model question is
asked for it. A root the walk does not reach, or a walk the cap trimmed, keeps
the asked site.

| Site | Form | Read today | Read after |
|---|---|---|---|
| `src/a-records.ts:7` | record fields through a class method, three callers | name unanswered | name `unread_expression` |
| `src/b-one-caller.ts:6` | one caller | name unanswered | name `unread_expression` |
| `src/c-entry-param.ts:6` | fields of an uncalled entry's parameter | name unanswered | name `unread_expression` |
| `src/d-no-caller.ts:6` | exported, no caller | name unanswered | name `name_never_bound` |
| `src/e-literal.ts:6` | literal objects: name stated, kind asked | kind unanswered | kind unanswered |
| `src/f-whole-field.ts:10`, `:15` | pooled whole field (carrick#4450) | name `unread_expression` | same |
| `src/g-mixed.ts:6` | one literal caller, one record caller | name unanswered | name `unread_expression` |
| `src/h-ctor.ts:10` | through a constructor body | name unanswered | name `unread_expression` |
| `src/i-half-reached.ts:9` | one reached root, one entry-point instance | name unanswered | name unanswered (follow-up carrick#4592) |
| `src/k-callback.ts:8` | a callback's own parameter | name unanswered | name `callback_parameter` |

Protocol: pub/sub (queues); the rule is protocol-free (any maker-bound op).

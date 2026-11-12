# pubsub-template-hole

A pub/sub topic written as a template whose hole is a parameter of the
function it is written in, where the callers that fill the parameter are
resolver methods a framework calls with request arguments (carrick#3784).
Nothing in the service calls the resolvers, so the topic is never text the
code states.

The service publishes through its own wrapper (`src/events/bus.service.ts`):
the hub is made in a lifecycle method and every topic is the wrapper's
parameter.

Three template shapes, one per file:

| File | Template | Callers that fill the hole |
|---|---|---|
| `src/settings/settings.service.ts` | `setting/${name}/changed` | a resolver argument, a callback's parameter inside another resolver (written first), and an enum member from a sibling method |
| `src/boards/board.service.ts` | `board_root/${boardId}/reordered`, `board_child/${boardId}/reordered` (one hole, two branches) | a resolver argument, from two resolvers |
| `src/members/members.service.ts` | `owner/${ownerId}/invited` | a member of a resolver's decorated parameter, and a call result in another service class |

Answer key:

- Each template call (four calls) is one saved pub/sub producer site at its
  own line, in both `possible_calls` and `unread_sites`, with the reason the
  walk met at the callers that never fill the hole. Before carrick#3784
  none of them was saved: the reason sat only at the resolver frame.
  - Board templates: two resolvers stop for the same reason, one entry,
    `name_never_bound`.
  - Members template: `name_never_bound`.
  - Settings template: its callers stop for different reasons. Both
    lists keep one entry per call, the first stop the walk meets; sites are
    walked in source order, so it is the first caller's reason,
    `callback_parameter` (`enableAll`). Each caller keeps its own entry and
    reason.
- No pub/sub row states a topic with a hole in it (`{`, `${`, `:`): pub/sub
  topics pair by exact text, so a placeholder would pair on its spelling.
- The resolver frames keep their own saved sites.
- Controls, unchanged: `board/created` (a literal through the wrapper) is
  one publisher row; `board/${boardId}/archived` (a hole read from a local
  holding a call result) stays saved at its own line as
  `unread_expression`.

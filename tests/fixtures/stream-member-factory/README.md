# stream-member-factory

carrick-cloud#3534 T4 (contract amendment 12). One service at the repo root.
The store labels `Hub.channel(name)` as a maker called on a `Hub` instance
(`of: instance:new`); `Channel.publish` is a broker send named by its maker.
Answer key, `repo/src/forms.ts` (a mock scan asks the broker kind, so a send
whose name is read ends `unanswered` on slot `kind`):

| Line | Form | Answer |
|---|---|---|
| 9 | factory returns the member call, literal at the caller | `unanswered`, slot `kind`; name `orders` |
| 20 | async factory, awaited | slot `kind`; name `audit` |
| 27 | held locals | slot `kind`; name `held` |
| 34 | module-scope constant | slot `kind`; name `shared` |
| 46 | name from a runtime value | `unread_expression`, slot `name` |
| 57 | in-repo class with a `channel` member (control) | `unanswered`, slot `name` |
| 62 | in-place chain | slot `kind`; name `chained` |

The sibling-package form of line 9 moves only with the sibling-ask work and is
added by whichever change lands last.

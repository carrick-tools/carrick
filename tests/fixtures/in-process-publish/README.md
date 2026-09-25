# `in-process-publish`

Fixture for carrick#1513: a service whose `publish(...)` calls go into its own
code, where some reach a broker and some never leave the process. Driven by
`tests/in_process_publish_test.rs`.

The `__llm__/analyze-file` answers report every `publish`/`subscribe` call as
a pub/sub operation, the way the file-analyzer does when it reads only the
caller's file. The scan decides from the wrapper's own body which rows stay.
Framework detection (`__llm__/framework-detect`) lists `@fixture/broker` as
the one messaging client; `@fixture/streams` and `@fixture/jobs` are declared
dependencies it does not list. No package is installed: the scan only needs
`package.json` to say they are dependencies.

## Withdrawn (in-process, nothing on the other side of the topic)

| Site | Wrapper | Why it is in-process |
|---|---|---|
| `orders.service.ts:23` | `FeedService.publish` | sends to a field constructed, with no arguments, from `@fixture/streams` |
| `orders.service.ts:25` | `FeedService.publish` | the same, as the second call from one function (the call graph keeps the first site only) |
| `orders.service.ts:30` | `AuditFeed.publish` | sends to the repo's own `LocalBus`, which runs listeners from an empty-initialised list |
| `orders.service.ts:33` | `notify` | a plain function sending to a module-scope instance |
| `inventory.listener.ts:10` | `FeedService.subscribe` | the subscribing side, on a line where the callback is a definition of its own |
| `billing.service.ts:24` (`invoice.viewed`) | `FeedService.publish` | the same wrapper from a second caller file |
| `audit.service.ts:14` | `FeedService.publish` | the same, with the topic passed as a module-scope constant |
| `audit.service.ts:15` | `FeedService.publish` | the same, with the topic in a template literal |

## Kept

| Site | Why |
|---|---|
| `orders.service.ts:24` | `BrokerPublisher` constructs its client with arguments |
| `orders.service.ts:31` | `RelayPublisher` sends through a transport it was handed |
| `orders.service.ts:32` | a call on the package client itself resolves to no repo function |
| `orders.service.ts:26`, `inventory.listener.ts:9` | in-process, but the service subscribes to the topic it publishes: a pair |
| `shipping.service.ts:15` | `SocketPublisher` constructs a runtime global (a socket) |
| `shipping.service.ts:16` | `HttpEvents` calls a global with arguments (`fetch`), beside a push onto its own list |
| `shipping.service.ts:17` | `SwappableFeed` reassigns the field it sends to |
| `shipping.service.ts:18` | `SyncedStore` calls nothing; it writes to an object it was handed |
| `billing.service.ts:18` | `DefaultBrokerPublisher` constructs the listed messaging client with no arguments |
| `billing.service.ts:19` | `JobPublisher` constructs an unlisted package's client with arguments |
| `billing.service.ts:20` | `scheduleJob` calls a function imported from a package |
| `billing.service.ts:21` | `EventPublisher` is in-memory, but `BrokerEventPublisher` extends it two levels down and overrides `publish` |
| `billing.service.ts:22` | `LedgerFeed` is in-memory, but `RemoteLedgerFeed` implements it with a network call |
| `billing.service.ts:23`, `refunds.listener.ts:5` | in-process, but an emitter in the service listens for the topic |
| `billing.service.ts:24` (`invoice.scheduled`) | the model placed line 20's row on line 24, whose call publishes a different topic |
| `streaming.service.ts:12` | `StreamBus` calls a method it inherits from a package class |
| `streaming.service.ts:13` | `queueNote` calls through a namespace import of a package |
| `streaming.service.ts:14` | `SnapshotPublisher` constructs a package object with arguments in its body, beside a push onto its own list |
| `audit.service.ts:13` | `AuditTrail` is in-memory, but `RetryingAuditTrail` defines the same member and extends a mixin call, an ancestry this pass cannot follow |

`audit.service.ts` also makes an unrelated `fetch` so the file raises a
candidate and the model is asked about it. `AuditTrail`'s member is `record`,
not `publish`, on purpose: a class whose ancestry cannot be followed keeps
every row whose wrapper member shares its name, so a `publish` there would
keep all of the `publish` rows above.

With the detection lists empty, the `inventory.reserved` pair, the
`streaming.service.ts` rows and `billing.service.ts:18` are the only rows
above that go. The listener file and `streaming.service.ts` raise no candidate
then, so the model is not asked about them and there is no row, and
`billing.service.ts:18` is the no-argument transport that only detection can
name. Every other kept row stays.

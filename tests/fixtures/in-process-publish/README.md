# `in-process-publish`

Fixture for carrick#1513: a service whose `publish(...)` calls go into its own
code, where some reach a broker and some never leave the process. Driven by
`tests/in_process_publish_test.rs`.

The `__llm__/analyze-file` answers report every `publish`/`subscribe` call as
a pub/sub operation, the way the file-analyzer does when it reads only the
caller's file. The scan decides from the wrapper's own body which rows stay.
Framework detection (`__llm__/framework-detect`) lists `@fixture/broker` as
the one messaging client; `@fixture/streams` is a declared dependency it does
not list. Neither package is installed: the scan only needs `package.json` to
say they are dependencies.

## Withdrawn (in-process, nothing on the other side of the topic)

| Site | Wrapper | Why it is in-process |
|---|---|---|
| `orders.service.ts:23` | `FeedService.publish` | sends to a field constructed from `@fixture/streams` |
| `orders.service.ts:25` | `FeedService.publish` | the same, as the second call from one function (the call graph keeps the first site only) |
| `orders.service.ts:30` | `AuditFeed.publish` | sends to the repo's own `LocalBus`, which runs listeners from an empty-initialised list |
| `orders.service.ts:33` | `notify` | a plain function sending to a module-scope instance |
| `inventory.listener.ts:10` | `FeedService.subscribe` | the subscribing side, on a line where the callback is a definition of its own |

## Kept

| Site | Why |
|---|---|
| `orders.service.ts:24` | `BrokerPublisher`'s module imports the detected messaging client |
| `orders.service.ts:31` | `RelayPublisher` sends through a transport it was handed |
| `orders.service.ts:32` | a call on the package client itself resolves to no repo function |
| `orders.service.ts:26`, `inventory.listener.ts:9` | in-process, but the service subscribes to the topic it publishes: a pair |
| `shipping.service.ts:13` | `SocketPublisher` constructs a runtime global (a socket) |
| `shipping.service.ts:14` | `HttpEvents` calls a global with arguments (`fetch`) |
| `shipping.service.ts:15` | `SwappableFeed` reassigns the field it sends to |

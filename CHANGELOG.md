# Changelog

## [0.3.4](https://github.com/carrick-tools/carrick/compare/carrick-v0.3.3...carrick-v0.3.4) (2026-07-27)


### Features

* **routing:** derive endpoints for flat file-based route modules ([#476](https://github.com/carrick-tools/carrick/issues/476)) ([c2d951f](https://github.com/carrick-tools/carrick/commit/c2d951fa322b2959555a748df2c40ab2a25b21ab)), closes [#473](https://github.com/carrick-tools/carrick/issues/473)


### Bug Fixes

* **scanner:** follow re-export barrels when resolving wrapper imports ([#475](https://github.com/carrick-tools/carrick/issues/475)) ([01312aa](https://github.com/carrick-tools/carrick/commit/01312aa2de5dc257b2f58d9c3338663461283917)), closes [#472](https://github.com/carrick-tools/carrick/issues/472)

## [0.3.3](https://github.com/carrick-tools/carrick/compare/carrick-v0.3.2...carrick-v0.3.3) (2026-07-27)


### Features

* **scanner:** capture end_line for function definitions ([#459](https://github.com/carrick-tools/carrick/issues/459)) ([e7bbda5](https://github.com/carrick-tools/carrick/commit/e7bbda585f4241e6b82379d5a3d26596bd212ebd))


### Bug Fixes

* **cloud:** enable reqwest gzip so large-project reads inflate ([#471](https://github.com/carrick-tools/carrick/issues/471)) ([08c62b3](https://github.com/carrick-tools/carrick/commit/08c62b31170676935ae12f0811c03635a2cde7e0))
* fetch-depth: 0 for incremental Carrick scans (FAR-76) ([#453](https://github.com/carrick-tools/carrick/issues/453)) ([f5a11b7](https://github.com/carrick-tools/carrick/commit/f5a11b79e12b03a2b1b001537cd69691f0ad935b))
* **scanner:** distinguish force-analyzed drops from candidate-id misses ([#462](https://github.com/carrick-tools/carrick/issues/462)) ([733e0c1](https://github.com/carrick-tools/carrick/commit/733e0c112afe6026c2f5d5b69b7964dc1143da97))
* **scanner:** resolve NodeNext `.js` import specifiers to their TS source ([#469](https://github.com/carrick-tools/carrick/issues/469)) ([41b2e03](https://github.com/carrick-tools/carrick/commit/41b2e03568c49fe92c5c4e87f18bae4b6ed320dd)), closes [#468](https://github.com/carrick-tools/carrick/issues/468)
* **scanner:** strip a dependency-injected base from the consumer match key ([#467](https://github.com/carrick-tools/carrick/issues/467)) ([#470](https://github.com/carrick-tools/carrick/issues/470)) ([fb70d12](https://github.com/carrick-tools/carrick/commit/fb70d12c4fc366453a23b2d662671dca93aa485f))
* **verified:** emit honest per-endpoint type verdicts ([#455](https://github.com/carrick-tools/carrick/issues/455), [#456](https://github.com/carrick-tools/carrick/issues/456)) ([#457](https://github.com/carrick-tools/carrick/issues/457)) ([bccf74e](https://github.com/carrick-tools/carrick/commit/bccf74efafadaf087559644353f00158c7f4deed))


### Documentation

* fix stale "exported functions" index-scope claims ([#458](https://github.com/carrick-tools/carrick/issues/458)) ([092787d](https://github.com/carrick-tools/carrick/commit/092787da2a817a77246c686d10f29110dd0d7900))

## [0.3.2](https://github.com/carrick-tools/carrick/compare/carrick-v0.3.1...carrick-v0.3.2) (2026-07-22)


### Bug Fixes

* **type-compat:** verify Unknown-state sides with a resolved v2 surface ([#448](https://github.com/carrick-tools/carrick/issues/448)) ([f0fdffa](https://github.com/carrick-tools/carrick/commit/f0fdffa3ad8e0591e9e88e21d714e55ae56c776f))

## [0.3.1](https://github.com/carrick-tools/carrick/compare/carrick-v0.3.0...carrick-v0.3.1) (2026-07-21)


### Features

* **sidecar:** parse yarn-berry and bun text lockfiles for bare-checkout pins ([#434](https://github.com/carrick-tools/carrick/issues/434)) ([4045606](https://github.com/carrick-tools/carrick/commit/40456065067f815665aacae885217748464305ac)), closes [#430](https://github.com/carrick-tools/carrick/issues/430)


### Bug Fixes

* **evals:** fold carrick.json service names to corpus repo dirs in the edge join ([#431](https://github.com/carrick-tools/carrick/issues/431)) ([#435](https://github.com/carrick-tools/carrick/issues/435)) ([bc2ecbf](https://github.com/carrick-tools/carrick/commit/bc2ecbf223e04997fe253e22aed7b70e59bfe801))
* **scanner:** resolve `./types.js` imports to the `.ts` sibling ([#148](https://github.com/carrick-tools/carrick/issues/148)) ([#443](https://github.com/carrick-tools/carrick/issues/443)) ([007241b](https://github.com/carrick-tools/carrick/commit/007241b6e6830ff93d2ac6fa8baacad03a16405d))
* **sidecar:** abstain on wrapped response-envelope machinery ([#371](https://github.com/carrick-tools/carrick/issues/371)) ([#445](https://github.com/carrick-tools/carrick/issues/445)) ([53274af](https://github.com/carrick-tools/carrick/commit/53274af4720d4cd9f0b72d0d019c9ef09b6e6564))
* **sidecar:** contain stub poison + fix value-only and builder-chain anchors ([#438](https://github.com/carrick-tools/carrick/issues/438), [#439](https://github.com/carrick-tools/carrick/issues/439)) ([#442](https://github.com/carrick-tools/carrick/issues/442)) ([7271698](https://github.com/carrick-tools/carrick/commit/7271698117b490f1a496f79df8b26aef17c47b7d))
* **sidecar:** recover inline literal response types under an unresolvable generic ([#433](https://github.com/carrick-tools/carrick/issues/433)) ([#437](https://github.com/carrick-tools/carrick/issues/437)) ([5678a54](https://github.com/carrick-tools/carrick/commit/5678a540c66799d9e022b2c642e5523baa145661))

## [0.3.0](https://github.com/carrick-tools/carrick/compare/carrick-v0.2.9...carrick-v0.3.0) (2026-07-20)


### ⚠ BREAKING CHANGES

* type-compat v2 — tsc as serializer and judge, ts_check deleted (the flip) ([#429](https://github.com/carrick-tools/carrick/issues/429))

### Features

* **evals:** install_corpus_deps dispatch input for bare-checkout eval arms ([#424](https://github.com/carrick-tools/carrick/issues/424)) ([3fd771d](https://github.com/carrick-tools/carrick/commit/3fd771d42e5f9c6ad3ec9b6ab05889b6f7654257))
* type-compat v2 — tsc as serializer and judge, ts_check deleted (the flip) ([#429](https://github.com/carrick-tools/carrick/issues/429)) ([5c889d2](https://github.com/carrick-tools/carrick/commit/5c889d260735d868ac5cf427e185c26bd49c53f0))

## [0.2.9](https://github.com/carrick-tools/carrick/compare/carrick-v0.2.8...carrick-v0.2.9) (2026-07-20)


### Features

* fail fast on Deno-native projects + inclusive requirements copy ([#366](https://github.com/carrick-tools/carrick/issues/366)) ([cc5e4b4](https://github.com/carrick-tools/carrick/commit/cc5e4b4542f11b912bb9e617431ab062935ddf19))
* pub/sub payload type capture via expression locators ([#386](https://github.com/carrick-tools/carrick/issues/386)) ([5e767d4](https://github.com/carrick-tools/carrick/commit/5e767d4658b2587a5312e8414aeb7089a9ef4940))
* **scanner:** deterministic subscriber anchors for object-literal, constructor, and two-arg subscribe shapes ([#409](https://github.com/carrick-tools/carrick/issues/409)) ([d78deaa](https://github.com/carrick-tools/carrick/commit/d78deaa239e5dbe638b1658d32fe238a3d54fd38))
* **scanner:** two-anchor arbitration for pub/sub payload anchors (witnessed borrow demotion) ([#414](https://github.com/carrick-tools/carrick/issues/414)) ([69cc7dd](https://github.com/carrick-tools/carrick/commit/69cc7dd0e50d8d98809808042e040b75b0298543))
* surface web-platform postMessage channel as pub/sub candidates ([#372](https://github.com/carrick-tools/carrick/issues/372)) ([75f5619](https://github.com/carrick-tools/carrick/commit/75f5619b800e657cc8010c6d78b37fb831adcae7))
* tag endpoint evidence with provenance (route vs mock/test handler) ([#384](https://github.com/carrick-tools/carrick/issues/384)) ([41b1b55](https://github.com/carrick-tools/carrick/commit/41b1b550281159b65ffb3d7a3df861588e3c6201))


### Bug Fixes

* **analyzer:** drop HTTP producer==consumer self-pair edges ([#410](https://github.com/carrick-tools/carrick/issues/410)) ([1fbbaa7](https://github.com/carrick-tools/carrick/commit/1fbbaa7adc39d99e1eb53bd1a81508614e36f861))
* apply symbol validation parity to pub/sub operations ([#406](https://github.com/carrick-tools/carrick/issues/406)) ([69d74b8](https://github.com/carrick-tools/carrick/commit/69d74b868ab7e2451d9130dd4f5521320cea33a1))
* backfill payload-less pub/sub ops from deterministic AST anchors ([#389](https://github.com/carrick-tools/carrick/issues/389)) ([26e4945](https://github.com/carrick-tools/carrick/commit/26e49451cf12142bd6e9d0ff9dc253d9752f5188))
* carry the member property on URL-scheme and awaited-call candidates ([#398](https://github.com/carrick-tools/carrick/issues/398)) ([23ac705](https://github.com/carrick-tools/carrick/commit/23ac705c071f77216642d3131dfe7225a23377e2))
* classify call-site-only match pairs as shared external contracts ([#385](https://github.com/carrick-tools/carrick/issues/385)) ([f29e095](https://github.com/carrick-tools/carrick/commit/f29e095ec6170880a541d5e9df3d7a254d051639))
* collapse inline env-var fallbacks in data-call targets ([#401](https://github.com/carrick-tools/carrick/issues/401)) ([75f8eee](https://github.com/carrick-tools/carrick/commit/75f8eee2c8dd7af5ba3794d2147d77be4aceeb8a))
* degrade gracefully when a type-check dependency is unresolvable ([#391](https://github.com/carrick-tools/carrick/issues/391)) ([99cb2ab](https://github.com/carrick-tools/carrick/commit/99cb2abf3ee7117b4540c5e0734285d185998bb8)), closes [#390](https://github.com/carrick-tools/carrick/issues/390)
* drop LLM pub/sub ops whose topic has no literal witness in the file ([#395](https://github.com/carrick-tools/carrick/issues/395)) ([2283bb2](https://github.com/carrick-tools/carrick/commit/2283bb22e25f2f6e78c7a8afcc9b65e774d0662e))
* fall back to backing type when graphql resolver_line is unusable ([#404](https://github.com/carrick-tools/carrick/issues/404)) ([5095d3f](https://github.com/carrick-tools/carrick/commit/5095d3fb22592da6956677bab40f1b3914ac7af9))
* fan mount-alias prefixes out into one endpoint per alias ([#393](https://github.com/carrick-tools/carrick/issues/393)) ([b740898](https://github.com/carrick-tools/carrick/commit/b7408983ab71e9bec6242a6972b2bb8d4421111c)), closes [#373](https://github.com/carrick-tools/carrick/issues/373)
* hash repo-relative call-site paths so scanner output is machine-independent ([#394](https://github.com/carrick-tools/carrick/issues/394)) ([8701fa1](https://github.com/carrick-tools/carrick/commit/8701fa1757406a0a7805296c8b30d7fc8777877d)), closes [#355](https://github.com/carrick-tools/carrick/issues/355)
* match ignore patterns against path segments, not substrings ([#382](https://github.com/carrick-tools/carrick/issues/382)) ([64cdf17](https://github.com/carrick-tools/carrick/commit/64cdf171bd956e2ebe1d2ea3be9a713a125e3944))
* **match:** gate cross-repo pairing on literal path agreement ([#383](https://github.com/carrick-tools/carrick/issues/383)) ([4443280](https://github.com/carrick-tools/carrick/commit/4443280dbb4fb382148ffd07bbb1587297c35868))
* name both sides of cross-repo match rows by the service-qualified id ([#396](https://github.com/carrick-tools/carrick/issues/396)) ([9ac5202](https://github.com/carrick-tools/carrick/commit/9ac52023000545f89dce2860acd01209b43ec8cf)), closes [#368](https://github.com/carrick-tools/carrick/issues/368)
* resolve env-var aliases through imported config-object properties ([#388](https://github.com/carrick-tools/carrick/issues/388)) ([319b497](https://github.com/carrick-tools/carrick/commit/319b497970046fc9611227f8756c00b130bec776))
* **scanner:** count all six response sections in result_score ([#412](https://github.com/carrick-tools/carrick/issues/412)) ([b5a8c73](https://github.com/carrick-tools/carrick/commit/b5a8c730439b2a198d739591974f411e1db7755a))


### Documentation

* add estimation conventions in agent execution time to AGENTS.md ([#408](https://github.com/carrick-tools/carrick/issues/408)) ([7c5c002](https://github.com/carrick-tools/carrick/commit/7c5c002a4dca8bc4e25983877ec8c0d887835f11))
* document crates/carrick-match and its wasm release artifact ([#331](https://github.com/carrick-tools/carrick/issues/331)) ([ccb057d](https://github.com/carrick-tools/carrick/commit/ccb057d0f887dd09e697b6fc88eb3fd25b19707f))
* restructure docs/ into reference/archive with a map and eval runbook ([#392](https://github.com/carrick-tools/carrick/issues/392)) ([2b42b93](https://github.com/carrick-tools/carrick/commit/2b42b9314ca99a9399770954f5cdb2aa7583dca0))
* type-compat v2 design — compiler-native artifacts in a synthetic monorepo ([#299](https://github.com/carrick-tools/carrick/issues/299)) ([2e09a25](https://github.com/carrick-tools/carrick/commit/2e09a25c69ae549a04c0a2bfc1590a55cfc68b40))
* user-scoped MCP install; refresh agent guidance to the workflow loop ([#375](https://github.com/carrick-tools/carrick/issues/375)) ([5f71f68](https://github.com/carrick-tools/carrick/commit/5f71f685a0d11825c93888913ba16d435714b2fe))

## [0.2.8](https://github.com/daveymoores/carrick/compare/carrick-v0.2.7...carrick-v0.2.8) (2026-07-12)


### Features

* persist per-pair ts_check verdicts into CloudRepoData ([#351](https://github.com/daveymoores/carrick/issues/351) part 1) ([#362](https://github.com/daveymoores/carrick/issues/362)) ([55904e5](https://github.com/daveymoores/carrick/commit/55904e5e5d4e13a3c74a83567e07d7ddede65500))


### Bug Fixes

* **extraction:** deterministic guards for request-type borrow + graphql-over-http target ([#361](https://github.com/daveymoores/carrick/issues/361)) ([#363](https://github.com/daveymoores/carrick/issues/363)) ([2ed6762](https://github.com/daveymoores/carrick/commit/2ed6762967180887b1f1a148e8d1c2044bb3bc00))

## [0.2.7](https://github.com/daveymoores/carrick/compare/carrick-v0.2.6...carrick-v0.2.7) (2026-07-12)


### Bug Fixes

* canonicalize literal-origin consumer call keys + suppress self-calls ([#354](https://github.com/daveymoores/carrick/issues/354)) ([47b1d45](https://github.com/daveymoores/carrick/commit/47b1d450ce55da6cb3b17d659630e8775992509e))
* **corpus-1:** label audit-webhook call + add resolveOrders resolver ([#358](https://github.com/daveymoores/carrick/issues/358)) ([418f106](https://github.com/daveymoores/carrick/commit/418f1061de39cb8749af90c3ae38bd59b1e0089e))
* **extraction:** normalize spurious .ts on type_import_source deterministically (flake pattern 1) ([#360](https://github.com/daveymoores/carrick/issues/360)) ([b68fc85](https://github.com/daveymoores/carrick/commit/b68fc851c9e3f591f00e6f04957f846cafec80b2))
* suppress intra-repo pub/sub self-loops in cross-repo projection ([#353](https://github.com/daveymoores/carrick/issues/353)) ([47cb1c1](https://github.com/daveymoores/carrick/commit/47cb1c1077309bfbf889e8926b4c4370f3ccea0f))

## [0.2.6](https://github.com/daveymoores/carrick/compare/carrick-v0.2.5...carrick-v0.2.6) (2026-07-11)


### Bug Fixes

* **sidecar:** anchor call_result from the explicit call generic when the client type is untyped ([#348](https://github.com/daveymoores/carrick/issues/348)) ([2831d6f](https://github.com/daveymoores/carrick/commit/2831d6fdb27e679e382c7053aa90e2cd984d50a4))

## [0.2.5](https://github.com/daveymoores/carrick/compare/carrick-v0.2.4...carrick-v0.2.5) (2026-07-11)


### Bug Fixes

* **sidecar:** tolerate SWC 1-based spans when binding a call_result locator ([#346](https://github.com/daveymoores/carrick/issues/346)) ([c8b74e4](https://github.com/daveymoores/carrick/commit/c8b74e4c9bc620aabb0aaab41a1814b113bd22b8))

## [0.2.4](https://github.com/daveymoores/carrick/compare/carrick-v0.2.3...carrick-v0.2.4) (2026-07-11)


### Bug Fixes

* **sidecar:** recover the consumer array depth for multi-line call sites ([#344](https://github.com/daveymoores/carrick/issues/344)) ([041e084](https://github.com/daveymoores/carrick/commit/041e08495f5fe9616f5f1730817257f790f2532c))

## [0.2.3](https://github.com/daveymoores/carrick/compare/carrick-v0.2.2...carrick-v0.2.3) (2026-07-11)


### Bug Fixes

* **comment:** polish contract-risk rows: runner paths, stray quote, declared param names ([#340](https://github.com/daveymoores/carrick/issues/340)) ([c844fc9](https://github.com/daveymoores/carrick/commit/c844fc9f9d27fb989b1095ec6cea08ab56c5e15f))
* **engine:** drop duplicate same-key producer manifest entries and dedup identical findings rows ([#342](https://github.com/daveymoores/carrick/issues/342)) ([d763245](https://github.com/daveymoores/carrick/commit/d7632457c5d7f3942c0e5949e2ed1d1eddcafb3b))
* **extraction:** sanitize LLM-emitted route paths and re-anchor them to the registration literal ([#343](https://github.com/daveymoores/carrick/issues/343)) ([1033890](https://github.com/daveymoores/carrick/commit/10338907f1d45949134ccd6ae7a9ccf2ff182804)), closes [#332](https://github.com/daveymoores/carrick/issues/332)
* **matching:** a param segment never matches an empty or whitespace segment ([#338](https://github.com/daveymoores/carrick/issues/338)) ([3e012be](https://github.com/daveymoores/carrick/commit/3e012bec0ad807c3ec41fc9290fe0acf0215c20b)), closes [#333](https://github.com/daveymoores/carrick/issues/333)
* **sidecar:** report element anchor + array_depth on call_result inference ([#341](https://github.com/daveymoores/carrick/issues/341)) ([fb6e8bb](https://github.com/daveymoores/carrick/commit/fb6e8bb62349987774953023240036843a666211))
* **sidecar:** trailing comma no longer defeats matchByText, reverse match needs 50% coverage ([#339](https://github.com/daveymoores/carrick/issues/339)) ([fd999e1](https://github.com/daveymoores/carrick/commit/fd999e191a94135866de8c4dbb3a98027ea4258f)), closes [#335](https://github.com/daveymoores/carrick/issues/335)


### CI/CD

* build carrick-match wasm artifact on release and dispatch pin bump to carrick-cloud ([#329](https://github.com/daveymoores/carrick/issues/329)) ([5975ebf](https://github.com/daveymoores/carrick/commit/5975ebf65b9a42746479fccd623ff664e496e025))

## [0.2.2](https://github.com/daveymoores/carrick/compare/carrick-v0.2.1...carrick-v0.2.2) (2026-07-10)


### Refactoring

* **matching:** extract carrick-match crate (paths_match + is_param_segment) with wasm feature ([#327](https://github.com/daveymoores/carrick/issues/327)) ([ecdbcfc](https://github.com/daveymoores/carrick/commit/ecdbcfc8d43ffc6d1db3a2b0dd315a55849ca6b5))

## [0.2.1](https://github.com/daveymoores/carrick/compare/carrick-v0.2.0...carrick-v0.2.1) (2026-07-10)


### Bug Fixes

* **action:** launch polish — fail loudly on missing id-token permission, stream scan output live ([#322](https://github.com/daveymoores/carrick/issues/322)) ([3fbfe6c](https://github.com/daveymoores/carrick/commit/3fbfe6cceee7794e3ec6de368a75b5aa3976f119))

## [0.2.0](https://github.com/daveymoores/carrick/compare/carrick-v0.1.40...carrick-v0.2.0) (2026-07-09)


### ⚠ BREAKING CHANGES

* **findings:** post-pr-comment relay removed; PR surfaces require the post-pr-result cloud pipeline.

### Features

* **compat:** type-check cross-repo SOCKET edges end-to-end ([#273](https://github.com/daveymoores/carrick/issues/273)) ([153d19b](https://github.com/daveymoores/carrick/commit/153d19b2976f771770bfdec8c5e87617c6b8c4ad))
* **eval:** add cross-repo metric fields to EvalRunRecord (S6 record-half) ([#212](https://github.com/daveymoores/carrick/issues/212)) ([0635707](https://github.com/daveymoores/carrick/commit/0635707fe359b282d1036364bd645fedf32ed219))
* **eval:** archive ts_check output + de-collapse projection by (key, role) ([#261](https://github.com/daveymoores/carrick/issues/261)) ([1e72fb7](https://github.com/daveymoores/carrick/commit/1e72fb76a7b626fe3b6d83d1fc93da125693c71b)), closes [#207](https://github.com/daveymoores/carrick/issues/207)
* **eval:** eval-oss workflow — remote corpus at pinned SHAs, dispatch-time labels ([#316](https://github.com/daveymoores/carrick/issues/316)) ([5367999](https://github.com/daveymoores/carrick/commit/5367999d4427e092fa92fae596cc263212f234f1))
* **eval:** full S4 cross-repo scorer — types/compat/deps/negatives/owner + GraphQL/socket ([#223](https://github.com/daveymoores/carrick/issues/223)) ([#225](https://github.com/daveymoores/carrick/issues/225)) ([b3ef66f](https://github.com/daveymoores/carrick/commit/b3ef66f5f5f969e965c3314a3893d32e89446485))
* **eval:** live cross-repo scorer — endpoint-set + match F1 (S4 thin slice) ([#215](https://github.com/daveymoores/carrick/issues/215)) ([cc868e1](https://github.com/daveymoores/carrick/commit/cc868e19258e0c33e9564c097a55ea59b21edda8))
* **eval:** LocalDirStorage + offline two-phase cross-repo harness (S2) ([#211](https://github.com/daveymoores/carrick/issues/211)) ([4def505](https://github.com/daveymoores/carrick/commit/4def50506c62c03aa38669da7c9f2a1008327991))
* **eval:** surface cross_repo_matches + type manifest + deps in EvalProjection (S1) ([#209](https://github.com/daveymoores/carrick/issues/209)) ([9f08ee6](https://github.com/daveymoores/carrick/commit/9f08ee6a56ae1366e6f04dba240c1c1c6a44f1f5))
* **eval:** xrepo-corpus-3 — messy-realism cross-repo corpus + kind-aware decoy scoring ([#303](https://github.com/daveymoores/carrick/issues/303)) ([5abd14e](https://github.com/daveymoores/carrick/commit/5abd14e00f6dd17afea6c7c9c8b36a5843a52621))
* **extraction:** pub/sub candidates are an exhaustive checklist for the analyzer ([#320](https://github.com/daveymoores/carrick/issues/320)) ([bf9d355](https://github.com/daveymoores/carrick/commit/bf9d355c2be974d5046b6f1327dc82df2b2f1ae4))
* **extraction:** route GraphQL/socket ops into the type pipeline ([#245](https://github.com/daveymoores/carrick/issues/245) Phase 1) ([#251](https://github.com/daveymoores/carrick/issues/251)) ([8253eaa](https://github.com/daveymoores/carrick/commit/8253eaa619f3d0f62f1dc0b3b51096aa1e1c0dc0))
* **findings:** typed findings + post-pr-result structured wire payload ([#313](https://github.com/daveymoores/carrick/issues/313)) ([a8a6e7e](https://github.com/daveymoores/carrick/commit/a8a6e7e7bf5cb04a6b8237d7444daf807b03db12))
* **graphql:** cross-repo compat checking (edge query|order → compatible) ([#278](https://github.com/daveymoores/carrick/issues/278)) ([39d1d61](https://github.com/daveymoores/carrick/commit/39d1d61637bcb6c5dda8e2a6bfa9e5c6a3c03c83))
* **graphql:** locate co-located consumer result types via file-analyzer hints ([#268](https://github.com/daveymoores/carrick/issues/268)) ([#298](https://github.com/daveymoores/carrick/issues/298)) ([107f41f](https://github.com/daveymoores/carrick/commit/107f41f9b313a9ed8509dde3060f17916b001055))
* **graphql:** resolve consumer bound type from client.request&lt;T&gt; call site ([#275](https://github.com/daveymoores/carrick/issues/275)) ([84d6d3d](https://github.com/daveymoores/carrick/commit/84d6d3dde6a3714321f3d1f153557bb12a82ffe9))
* **graphql:** resolve producer types from resolver functions (scanner-only) ([#276](https://github.com/daveymoores/carrick/issues/276)) ([3aa881e](https://github.com/daveymoores/carrick/commit/3aa881e729559d01033ea4532588c6694c21a222))
* **graphql:** resolve resolver-less SDL producers via co-located backing type ([#248](https://github.com/daveymoores/carrick/issues/248)) ([#296](https://github.com/daveymoores/carrick/issues/296)) ([6d91033](https://github.com/daveymoores/carrick/commit/6d910331898e3a18773b4cbe0b03d2d304cdfba3))
* **intents:** CARRICK_SKIP_INTENTS flag skips /generate-intent lambda calls ([#314](https://github.com/daveymoores/carrick/issues/314)) ([b829576](https://github.com/daveymoores/carrick/commit/b829576306699eaa27b36aec836df95e97dbc258))
* **intents:** repo-relative cloud paths, full-scan intent reuse, trivial-body gate ([#312](https://github.com/daveymoores/carrick/issues/312)) ([d8d8f1c](https://github.com/daveymoores/carrick/commit/d8d8f1c90db7300a40d3cf8f80532fa25c27ba32))
* pub/sub family protocol + xrepo-corpus-2 (event-driven accuracy corpus) ([#288](https://github.com/daveymoores/carrick/issues/288)) ([5c35fba](https://github.com/daveymoores/carrick/commit/5c35fba369b579c4000a6d0fc22904b1ddc2d1fd))
* **pubsub:** gated candidate-surfacing for pub/sub call sites ([#289](https://github.com/daveymoores/carrick/issues/289)) ([d2a8d2b](https://github.com/daveymoores/carrick/commit/d2a8d2b69cd5fcf55134d1cae9871a4cee8082d5))
* **scanner:** two-tier Signal 7 gate — shape-gate injected/inherited messaging clients ([#317](https://github.com/daveymoores/carrick/issues/317)) ([#319](https://github.com/daveymoores/carrick/issues/319)) ([77bcc7e](https://github.com/daveymoores/carrick/commit/77bcc7e1c1dd948cd09103717cdadccaa825271b))
* **schema:** trace-rule primary_type_symbol descriptions (anchor accuracy) ([#258](https://github.com/daveymoores/carrick/issues/258)) ([9629ed8](https://github.com/daveymoores/carrick/commit/9629ed8256a7928133637e6ee4ed670606903c43))


### Bug Fixes

* **compat:** key type-compat verdicts per consumer, not per producer ([#260](https://github.com/daveymoores/carrick/issues/260)) ([#264](https://github.com/daveymoores/carrick/issues/264)) ([9f3ed50](https://github.com/daveymoores/carrick/commit/9f3ed50dba4c5cfac8fec7fb5e32fb9fbef0e0aa))
* **compat:** normalize path params in the verdict-join so ts_check verdicts reach the score ([#271](https://github.com/daveymoores/carrick/issues/271)) ([a1d071e](https://github.com/daveymoores/carrick/commit/a1d071ecc311f10d1f41def337cab33594243157))
* **engine:** exclude workspace-internal packages from the synthetic type-check npm install ([#308](https://github.com/daveymoores/carrick/issues/308)) ([f251fe6](https://github.com/daveymoores/carrick/commit/f251fe6b3ff3b914d340e25556877c81ad0a24df))
* **engine:** harden synthetic type-check install — no lifecycle scripts, no non-registry version protocols ([#318](https://github.com/daveymoores/carrick/issues/318)) ([6ae0fde](https://github.com/daveymoores/carrick/commit/6ae0fdeb9a149ee6cebe25b9397ab70927aa01f5))
* **engine:** per-service cross-repo .d.ts bundles so monorepo producer types survive ([#270](https://github.com/daveymoores/carrick/issues/270)) ([195be31](https://github.com/daveymoores/carrick/commit/195be3109774283f166000c5673a5c249a6a6345))
* **engine:** scope git invocations to repo_path (ignore ambient GIT_DIR) ([#210](https://github.com/daveymoores/carrick/issues/210)) ([32f20ea](https://github.com/daveymoores/carrick/commit/32f20ea7bc02778cc8c14c4909639fa3cb1c75f3))
* **eval:** GraphQL/socket cross-repo edges + real type-anchor & resolution metrics ([#232](https://github.com/daveymoores/carrick/issues/232), [#233](https://github.com/daveymoores/carrick/issues/233)) ([#239](https://github.com/daveymoores/carrick/issues/239)) ([b4376f5](https://github.com/daveymoores/carrick/commit/b4376f5c89de9d923d3a73229e1e3e043543601c))
* **eval:** produce cross-repo compat verdicts in the offline harness + non-blocking §7 guard ([#226](https://github.com/daveymoores/carrick/issues/226)) ([#231](https://github.com/daveymoores/carrick/issues/231)) ([729d9ed](https://github.com/daveymoores/carrick/commit/729d9edbf7d6f7f8db1c24ab8d209d8456f7d090))
* **eval:** projection type-slot correctness + sidecar request-body locator convergence ([#297](https://github.com/daveymoores/carrick/issues/297)) ([a7ebb41](https://github.com/daveymoores/carrick/commit/a7ebb41e9a37fb7809f000fc16839cdb8f3eeb5a))
* **eval:** resolve spurious GraphQL 'orders' query ([#228](https://github.com/daveymoores/carrick/issues/228)) ([#229](https://github.com/daveymoores/carrick/issues/229)) ([d654417](https://github.com/daveymoores/carrick/commit/d65441743cfd9149be1ebdfc918e1a423e63dfdb))
* **extraction:** deterministically extract route-descriptor endpoints ([#234](https://github.com/daveymoores/carrick/issues/234)) ([#237](https://github.com/daveymoores/carrick/issues/237)) ([84a74a8](https://github.com/daveymoores/carrick/commit/84a74a8e5fc1d9ad295ff925ed1bd08a0220e261))
* **extraction:** extract raw-handler GET /gateway/health producer ([#227](https://github.com/daveymoores/carrick/issues/227)) ([#230](https://github.com/daveymoores/carrick/issues/230)) ([56e0453](https://github.com/daveymoores/carrick/commit/56e0453ff80dc001caa8b9640d3a320345f069f5))
* **extraction:** force pubsub + data-call locator emission via schema required-ness (lever D) ([#300](https://github.com/daveymoores/carrick/issues/300)) ([e43c06d](https://github.com/daveymoores/carrick/commit/e43c06dfc73987ba713acbf176fba47aebb85926))
* **extraction:** resolve env-var base URLs aliased through a local const ([#218](https://github.com/daveymoores/carrick/issues/218)) ([#219](https://github.com/daveymoores/carrick/issues/219)) ([b47e408](https://github.com/daveymoores/carrick/commit/b47e4081ea52fd14e7376a694ab2bd42fda37352))
* **extraction:** suppress non-matchable call noise (templated wrapper paths + graphql transport POSTs) ([#310](https://github.com/daveymoores/carrick/issues/310)) ([d981cb4](https://github.com/daveymoores/carrick/commit/d981cb4cca0f076e2280af9791fa4515e8cbe52d))
* **file_finder:** exclude storybook stories and .storybook config from scans ([#315](https://github.com/daveymoores/carrick/issues/315)) ([3c94ffc](https://github.com/daveymoores/carrick/commit/3c94ffc30b1177145c533a3ecf3811afc0f66e32))
* **graphql:** deterministic SDL type anchor for producers ([#248](https://github.com/daveymoores/carrick/issues/248)) ([#269](https://github.com/daveymoores/carrick/issues/269)) ([d6c196b](https://github.com/daveymoores/carrick/commit/d6c196b4cfebe7343469ffa000e25066e878fced))
* **graphql:** scope SDL discovery to the service's own roots ([#242](https://github.com/daveymoores/carrick/issues/242)) ([#252](https://github.com/daveymoores/carrick/issues/252)) ([ab8292b](https://github.com/daveymoores/carrick/commit/ab8292ba8dbe35d93920acc47bff7adab4267767))
* **hooks:** isolate git env in pre-commit + block on test failure ([#216](https://github.com/daveymoores/carrick/issues/216)) ([692ed97](https://github.com/daveymoores/carrick/commit/692ed9776106cf0758cbade22b025201901f9966))
* keep ts_check manifests HTTP-only so compat verdicts survive non-HTTP entries ([#254](https://github.com/daveymoores/carrick/issues/254)) ([99d57f7](https://github.com/daveymoores/carrick/commit/99d57f73a673230f821aa2313e117644ac071223))
* **orchestrator:** resolve bare import specifiers through tsconfig paths mappings ([#305](https://github.com/daveymoores/carrick/issues/305)) ([11a7374](https://github.com/daveymoores/carrick/commit/11a73747a4dcf96b6137fab4f7445c66413f69dc))
* **pubsub:** disambiguate fan-in publisher aliases by call site ([#290](https://github.com/daveymoores/carrick/issues/290)) ([57896e7](https://github.com/daveymoores/carrick/commit/57896e7f304a3673bedb452239b7b15ea872a27d))
* **scanner:** gate deterministic route descriptors to real registries ([#241](https://github.com/daveymoores/carrick/issues/241)) ([#249](https://github.com/daveymoores/carrick/issues/249)) ([dcf51d1](https://github.com/daveymoores/carrick/commit/dcf51d19e7f1df0c196a5c06a976831b2e2524ed)), closes [#207](https://github.com/daveymoores/carrick/issues/207)
* **schema:** require endpoint payload locators; scope type-slot to response ([#274](https://github.com/daveymoores/carrick/issues/274)) ([df4594d](https://github.com/daveymoores/carrick/commit/df4594dabd7cdeb5bbebdd0a9c51d647c4ba8c30))
* semver-incompatible dep conflicts + drop synthetic anchor fallback (eval 85%→91%) ([#279](https://github.com/daveymoores/carrick/issues/279)) ([f20dd41](https://github.com/daveymoores/carrick/commit/f20dd410993b17592eb24a728cf3e20bed8a4e07))
* **sidecar:** drill through JSON.stringify(arg) for request-body inference ([#267](https://github.com/daveymoores/carrick/issues/267)) ([552a07b](https://github.com/daveymoores/carrick/commit/552a07b795d86dba5f94e5d5deb807aa922d307e))
* **sidecar:** expand inferred consumer types structurally ([#257](https://github.com/daveymoores/carrick/issues/257)) ([#259](https://github.com/daveymoores/carrick/issues/259)) ([88f83af](https://github.com/daveymoores/carrick/commit/88f83af05e566485c37e2d522b2531f9ec04284f))
* **sidecar:** expand resolved request-body type structurally ([#272](https://github.com/daveymoores/carrick/issues/272)) ([331e8c2](https://github.com/daveymoores/carrick/commit/331e8c296ab178caae06d03fa9318616cd4dd8ff))
* **sidecar:** follow route-registration into handler for type inference (incl. line-only anchors) ([#295](https://github.com/daveymoores/carrick/issues/295)) ([12e1464](https://github.com/daveymoores/carrick/commit/12e1464b21b085af929780f23e74fb31602d0cb9))
* **sidecar:** inline named member types in expanded_definition ([#246](https://github.com/daveymoores/carrick/issues/246)) ([#255](https://github.com/daveymoores/carrick/issues/255)) ([4216a2e](https://github.com/daveymoores/carrick/commit/4216a2eef03bd77bd9f756cde6e7740da854113e))
* **sidecar:** producer structural expansion + deterministic type anchor ([#265](https://github.com/daveymoores/carrick/issues/265)) ([61ae9bc](https://github.com/daveymoores/carrick/commit/61ae9bcbe75bcf5aec4bfaf44e0d2acc8adb512f))
* **ts_check:** abstain instead of false-compatible on dangling library types (MVP safety) ([#302](https://github.com/daveymoores/carrick/issues/302)) ([79059d1](https://github.com/daveymoores/carrick/commit/79059d17641a057ac5cd4861bb16f76ae6f173cb))
* **ts_check:** correct HTTP request-body assignability direction (+ eval pins) ([#301](https://github.com/daveymoores/carrick/issues/301)) ([553f0a4](https://github.com/daveymoores/carrick/commit/553f0a491a350a1d62f5b07e21b03e68a4212e7c))
* **ts_check:** tag injected `= unknown` placeholder so genuine unknown isn't downgraded ([#244](https://github.com/daveymoores/carrick/issues/244)) ([#250](https://github.com/daveymoores/carrick/issues/250)) ([925fd0c](https://github.com/daveymoores/carrick/commit/925fd0c294fa835871aa3a7dc035e0309267f7dd)), closes [#207](https://github.com/daveymoores/carrick/issues/207)
* **ts_check:** treat unknown as unverifiable + land consumer type shape in bundle ([#235](https://github.com/daveymoores/carrick/issues/235)) ([#238](https://github.com/daveymoores/carrick/issues/238)) ([6361d7f](https://github.com/daveymoores/carrick/commit/6361d7f3a351afd2c29f9232f68099002025f326)), closes [#207](https://github.com/daveymoores/carrick/issues/207)
* **types:** keep use-site array-ness on explicit anchor bundles ([#309](https://github.com/daveymoores/carrick/issues/309)) ([20386e2](https://github.com/daveymoores/carrick/commit/20386e2bbd7a0d8df1b8d0cc9c129b6c87dc0815))


### Refactoring

* single canonical consumer-call key (kill conflicting normalization paths) ([#294](https://github.com/daveymoores/carrick/issues/294)) ([954485c](https://github.com/daveymoores/carrick/commit/954485cbadaf4649026c68bcbac8d8ef1058e31f))


### CI/CD

* **eval:** push Tier-A records to cloud eval history ([#198](https://github.com/daveymoores/carrick/issues/198)) ([479785b](https://github.com/daveymoores/carrick/commit/479785bb1914853b93b90ada0de6f6d39b1dbae2))
* run the ts_check test suite in CI ([#280](https://github.com/daveymoores/carrick/issues/280)) ([3045a68](https://github.com/daveymoores/carrick/commit/3045a6869315e00c553e4545168bb26aa6fee8c3))

## [0.1.40](https://github.com/daveymoores/carrick/compare/carrick-v0.1.39...carrick-v0.1.40) (2026-06-24)


### Bug Fixes

* **analyzer:** canonical endpoint/call ordering + cassette hard gate ([#195](https://github.com/daveymoores/carrick/issues/195)) ([49f94cf](https://github.com/daveymoores/carrick/commit/49f94cf7f442685769b6143d71c7d0d2759f137f))

## [0.1.39](https://github.com/daveymoores/carrick/compare/carrick-v0.1.38...carrick-v0.1.39) (2026-06-23)


### Performance

* **file-analyzer:** front-load stable guidance for implicit prompt caching ([#193](https://github.com/daveymoores/carrick/issues/193)) ([c30dc08](https://github.com/daveymoores/carrick/commit/c30dc08d8bf80e0807f4e01aa3d058e6db92af9b))

## [0.1.38](https://github.com/daveymoores/carrick/compare/carrick-v0.1.37...carrick-v0.1.38) (2026-06-23)


### Features

* **evals:** JSON output + Tier-A extraction-quality scorer ([#188](https://github.com/daveymoores/carrick/issues/188)) ([f1bd3d1](https://github.com/daveymoores/carrick/commit/f1bd3d1237835ba19c91273e27cc51d36d3e8895))
* **evals:** N-run variance + pass@k/pass^k ([#190](https://github.com/daveymoores/carrick/issues/190)) ([3414ce9](https://github.com/daveymoores/carrick/commit/3414ce98f75fe3b07a55c11c6f128006adf946b4))
* **evals:** Slice 3 store + capture + determinism fixes (resolver guard, scorer retry) ([#192](https://github.com/daveymoores/carrick/issues/192)) ([792bcc2](https://github.com/daveymoores/carrick/commit/792bcc24e6c16cc3e3116b33431436f1335767c6))


### Bug Fixes

* **deps:** bump quinn-proto to 0.11.15 (RUSTSEC-2026-0185) ([#191](https://github.com/daveymoores/carrick/issues/191)) ([76f74a3](https://github.com/daveymoores/carrick/commit/76f74a3af32bf703cbfaee418a37e8efd2be7a03))

## [0.1.37](https://github.com/daveymoores/carrick/compare/carrick-v0.1.36...carrick-v0.1.37) (2026-06-20)


### Features

* **extraction:** call_kind classification field (groundwork, [#129](https://github.com/daveymoores/carrick/issues/129)) ([359773e](https://github.com/daveymoores/carrick/commit/359773e41f46826819a50358ece6ea1abf6a7ab3))

## [0.1.36](https://github.com/daveymoores/carrick/compare/carrick-v0.1.35...carrick-v0.1.36) (2026-06-19)


### Bug Fixes

* **scanner:** extraction determinism + signature fidelity ([#183](https://github.com/daveymoores/carrick/issues/183)) ([9e5c837](https://github.com/daveymoores/carrick/commit/9e5c8373a15766aca1b9e5f4692588fefcdd657f))

## [0.1.35](https://github.com/daveymoores/carrick/compare/carrick-v0.1.34...carrick-v0.1.35) (2026-06-18)


### Documentation

* clarify ts_check is the active compatibility checker, not legacy ([#164](https://github.com/daveymoores/carrick/issues/164)) ([41f1453](https://github.com/daveymoores/carrick/commit/41f145300c0927ce5ab6fa8d13bbd15b7d5e0188))

## [0.1.34](https://github.com/daveymoores/carrick/compare/carrick-v0.1.33...carrick-v0.1.34) (2026-06-18)


### Bug Fixes

* fail fast on LLM quota exhaustion instead of 20-minute backoff storm ([#177](https://github.com/daveymoores/carrick/issues/177)) ([ab14e44](https://github.com/daveymoores/carrick/commit/ab14e44ea2078d40a65511c4d30a9c98094a50e3))

## [0.1.33](https://github.com/daveymoores/carrick/compare/carrick-v0.1.32...carrick-v0.1.33) (2026-06-18)


### Features

* **scanner:** emit dashboard badge + link placeholders in PR comments ([#176](https://github.com/daveymoores/carrick/issues/176)) ([cd17216](https://github.com/daveymoores/carrick/commit/cd172165138608ce64a127da187e9277934e9644))

## [0.1.32](https://github.com/daveymoores/carrick/compare/carrick-v0.1.31...carrick-v0.1.32) (2026-06-17)


### Features

* **scanner:** recognize navigator.sendBeacon as an HTTP POST call ([62a77d7](https://github.com/daveymoores/carrick/commit/62a77d76fab84b200ab9e518ebfd76dc0fb64975))


### Bug Fixes

* **scanner:** address Copilot review feedback from [#163](https://github.com/daveymoores/carrick/issues/163), [#171](https://github.com/daveymoores/carrick/issues/171), [#145](https://github.com/daveymoores/carrick/issues/145) ([#172](https://github.com/daveymoores/carrick/issues/172)) ([088c532](https://github.com/daveymoores/carrick/commit/088c532071cb6107b492ca1468767f03bd956c98))
* **scanner:** type-extraction fidelity quick wins — request-body cast unwrap + defaulted-union params ([1aff754](https://github.com/daveymoores/carrick/commit/1aff754b674cbd6fb3a97c2fbe17f2ec07c72e1d))

## [0.1.31](https://github.com/daveymoores/carrick/compare/carrick-v0.1.30...carrick-v0.1.31) (2026-06-17)


### Bug Fixes

* **action:** stop 404 body poisoning the release download URL ([#160](https://github.com/daveymoores/carrick/issues/160)) ([6de05f7](https://github.com/daveymoores/carrick/commit/6de05f798e43673828be27e4eab80afa6d840597))

## [0.1.30](https://github.com/daveymoores/carrick/compare/carrick-v0.1.29...carrick-v0.1.30) (2026-06-12)


### Features

* **agents:** route the LLM pipeline by protocol ([ef30fac](https://github.com/daveymoores/carrick/commit/ef30facee4bbc3b888e43417ee99950f62a24471))
* **graphql:** deterministic GraphQL contract extraction and matching ([5720bed](https://github.com/daveymoores/carrick/commit/5720bed18d8b3548b9175a7fbe2da92a9d03439f))
* multi-protocol contract indexing — GraphQL, Socket.IO, protocol-routed pipeline ([7a174bc](https://github.com/daveymoores/carrick/commit/7a174bcca15e071290c07e06fac10693f4133311))
* multi-protocol scanner expansion + cloud-protocol companions ([#158](https://github.com/daveymoores/carrick/issues/158)) ([90bf128](https://github.com/daveymoores/carrick/commit/90bf128b4b87affa5db6eea6a2c91b7fbc5b70c1))
* **socket:** deterministic Socket.IO contract extraction and matching ([9d7451f](https://github.com/daveymoores/carrick/commit/9d7451ff1b2e150d9451c19a9309f870d58eb703))


### Bug Fixes

* close scanner failure points — cloud resilience, fail-loud scans, degradation surfacing ([#152](https://github.com/daveymoores/carrick/issues/152)) ([b2f380e](https://github.com/daveymoores/carrick/commit/b2f380e3ed494a3a0cfe52f56e46ce9be0d51418))
* close type-extraction gaps confirmed by gap-regression tests ([#150](https://github.com/daveymoores/carrick/issues/150)) ([fd869dc](https://github.com/daveymoores/carrick/commit/fd869dc82e03e189d5a9ae814ff6583a1aa3ec30))


### Refactoring

* key endpoints and calls by protocol-tagged OperationKey ([b2ccbac](https://github.com/daveymoores/carrick/commit/b2ccbac034f9f3115cbd6327f138a4e436c282eb))
* key type-manifest entries by OperationKey across Rust and ts_check ([69d78d6](https://github.com/daveymoores/carrick/commit/69d78d6fe4fb822bd07e08c6de6d9a8c7f7015c2))


### Documentation

* **research:** cite mock-LLM fixture harness as protocol-phase regression net ([77a6e62](https://github.com/daveymoores/carrick/commit/77a6e62493652272c0bb297451df118d99c43092))
* **research:** deep dive on multi-protocol expansion (GraphQL, WebSockets, queues) ([42a9ccd](https://github.com/daveymoores/carrick/commit/42a9ccd9e369ba9161d9ee81418ea94e618468cd))
* **research:** drop compat ceremony from protocol expansion plan, add MVP brittleness guardrails ([e68447c](https://github.com/daveymoores/carrick/commit/e68447caf79d4bc679609627d8b9dc5079041dfe))
* **research:** protocol-routed LLM prompts design ([ccd35a8](https://github.com/daveymoores/carrick/commit/ccd35a8623d6bb11490675952365f7014a14b279))

## [0.1.29](https://github.com/daveymoores/carrick/compare/carrick-v0.1.28...carrick-v0.1.29) (2026-06-06)


### Bug Fixes

* key uploads by service; drop false-positive call extractions ([#143](https://github.com/daveymoores/carrick/issues/143)) ([802c5eb](https://github.com/daveymoores/carrick/commit/802c5ebf1327c2deb9c55cdec5b5603a5bc6bf9c))

## [0.1.28](https://github.com/daveymoores/carrick/compare/carrick-v0.1.27...carrick-v0.1.28) (2026-06-06)


### Features

* **intent:** content-hash caching for generate-intent ([#139](https://github.com/daveymoores/carrick/issues/139)) ([78e1f2a](https://github.com/daveymoores/carrick/commit/78e1f2a465a27c46ce8e85f38cfbea7c264e9505))

## [0.1.27](https://github.com/daveymoores/carrick/compare/carrick-v0.1.26...carrick-v0.1.27) (2026-06-04)


### Features

* **file-based-routing:** add Astro convention + real Next.js/Astro fixtures ([#131](https://github.com/daveymoores/carrick/issues/131)) ([4e0066c](https://github.com/daveymoores/carrick/commit/4e0066c944eb14445134eb8715186597c160af8a))
* multi-service (monorepo) support in carrick.json ([#135](https://github.com/daveymoores/carrick/issues/135)) ([a674cef](https://github.com/daveymoores/carrick/commit/a674cef500a85b933a54fa410cc99bc102af65c3))

## [0.1.26](https://github.com/daveymoores/carrick/compare/carrick-v0.1.25...carrick-v0.1.26) (2026-06-03)


### Features

* send workflow run_id with PR comment for cross-repo re-runs ([859fe8b](https://github.com/daveymoores/carrick/commit/859fe8b67d4b3f5b5b884aa78800bb65c3258e62))

## [0.1.25](https://github.com/daveymoores/carrick/compare/carrick-v0.1.24...carrick-v0.1.25) (2026-06-02)


### Features

* post PR drift comments from the cloud GitHub App ([8181c35](https://github.com/daveymoores/carrick/commit/8181c3508d4c54da9e4951a0194a67e9b33dec8a))

## [0.1.24](https://github.com/daveymoores/carrick/compare/carrick-v0.1.23...carrick-v0.1.24) (2026-05-31)


### Features

* framework-agnostic file-based routing ([#126](https://github.com/daveymoores/carrick/issues/126)) ([7939fe4](https://github.com/daveymoores/carrick/commit/7939fe42a5fa7dd4be1254614a3a49cf59e028b9))


### Performance

* **scanner:** parallelize file analysis and fix matching/extraction correctness ([#124](https://github.com/daveymoores/carrick/issues/124)) ([72b3ff1](https://github.com/daveymoores/carrick/commit/72b3ff1ed1912763aed0521ba4759bbc33bb66dd))

## [0.1.23](https://github.com/daveymoores/carrick/compare/carrick-v0.1.22...carrick-v0.1.23) (2026-05-29)


### Features

* **action:** keyless uploads via GitHub Actions OIDC ([#119](https://github.com/daveymoores/carrick/issues/119)) ([b166033](https://github.com/daveymoores/carrick/commit/b1660331078df0623223576b41a414d7a76bc230))

## [0.1.22](https://github.com/daveymoores/carrick/compare/carrick-v0.1.21...carrick-v0.1.22) (2026-05-29)


### Features

* **parser:** enable jsx parsing for .jsx files ([#114](https://github.com/daveymoores/carrick/issues/114)) ([77d1c1d](https://github.com/daveymoores/carrick/commit/77d1c1dbe0919854b02012046ed994ace088315c))
* **scanner:** ship function argument + return types to MCP ([#113](https://github.com/daveymoores/carrick/issues/113)) ([2bfd4b0](https://github.com/daveymoores/carrick/commit/2bfd4b09d31caf3df75a4f9c978c6adf61afbb3b))
* typed function signatures (scanner + sidecar) ([#118](https://github.com/daveymoores/carrick/issues/118)) ([46ee76c](https://github.com/daveymoores/carrick/commit/46ee76cbf216e04ae339993b55c68d1e5f756d31))

## [0.1.21](https://github.com/daveymoores/carrick/compare/carrick-v0.1.20...carrick-v0.1.21) (2026-05-10)


### Bug Fixes

* **intent:** generate intents in incremental path ([#110](https://github.com/daveymoores/carrick/issues/110)) ([#111](https://github.com/daveymoores/carrick/issues/111)) ([c2d5e1c](https://github.com/daveymoores/carrick/commit/c2d5e1c1204a4942f0d23bec5268d507d6c1db8e))
* **test:** integration parser counts endpoints from new "Indexed" header ([#108](https://github.com/daveymoores/carrick/issues/108)) ([de13909](https://github.com/daveymoores/carrick/commit/de13909c6d06559fc168c4a03f1dc475822da6ba))

## [0.1.20](https://github.com/daveymoores/carrick/compare/carrick-v0.1.19...carrick-v0.1.20) (2026-05-07)


### Bug Fixes

* **matcher+ux:** trim wrapper chars, surface verified matches, expose stages in CI ([#100](https://github.com/daveymoores/carrick/issues/100)) ([3d52671](https://github.com/daveymoores/carrick/commit/3d52671541c3af7f57e4cb1fc4a9b0cf5caff87e))

## [0.1.19](https://github.com/daveymoores/carrick/compare/carrick-v0.1.18...carrick-v0.1.19) (2026-05-06)


### Bug Fixes

* **formatter+resolver:** missing-endpoint prefix + tsconfig baseUrl resolution ([#95](https://github.com/daveymoores/carrick/issues/95)) ([80d18d8](https://github.com/daveymoores/carrick/commit/80d18d81d099d78c47ffff2b2e2cb83a8844aa91))


### Documentation

* **readme:** include carrick-sibling-updated dispatch trigger in example workflow ([#96](https://github.com/daveymoores/carrick/issues/96)) ([bde20e5](https://github.com/daveymoores/carrick/commit/bde20e5dea18bce968f9e386cd89c1fb46718d2c))

## [0.1.18](https://github.com/daveymoores/carrick/compare/carrick-v0.1.17...carrick-v0.1.18) (2026-05-06)


### Features

* **observability:** emit X-Carrick-Run-Id on every cloud call ([#93](https://github.com/daveymoores/carrick/issues/93)) ([6d17ea5](https://github.com/daveymoores/carrick/commit/6d17ea5ea414454220ea2d517387e4ff96adef57))

## [0.1.17](https://github.com/daveymoores/carrick/compare/carrick-v0.1.16...carrick-v0.1.17) (2026-05-06)


### Bug Fixes

* **logging:** unblock CI log uploads and improve diagnostics ([#91](https://github.com/daveymoores/carrick/issues/91)) ([35d3900](https://github.com/daveymoores/carrick/commit/35d390002baa72647977abb02d357f534870862f))

## [0.1.16](https://github.com/daveymoores/carrick/compare/carrick-v0.1.15...carrick-v0.1.16) (2026-05-04)


### Bug Fixes

* **release:** tag v1 from github.sha instead of release tag name ([#89](https://github.com/daveymoores/carrick/issues/89)) ([7fefe17](https://github.com/daveymoores/carrick/commit/7fefe1776d061a83cb3888e02b9fba3bf0ae7734))

## [0.1.15](https://github.com/daveymoores/carrick/compare/carrick-v0.1.14...carrick-v0.1.15) (2026-05-04)


### Bug Fixes

* **release:** handle release-please component-prefixed tags in v1 guard ([#87](https://github.com/daveymoores/carrick/issues/87)) ([f2a57b1](https://github.com/daveymoores/carrick/commit/f2a57b1cf100772b01051fa136e0ff15cf4890aa))

## [0.1.14](https://github.com/daveymoores/carrick/compare/carrick-v0.1.13...carrick-v0.1.14) (2026-05-04)


### CI/CD

* **release:** force-update [@v1](https://github.com/v1) moving tag on every stable release ([#85](https://github.com/daveymoores/carrick/issues/85)) ([b0fd90b](https://github.com/daveymoores/carrick/commit/b0fd90bafe74642453910cb32ded3c77f02e3d2b))

## [0.1.13](https://github.com/daveymoores/carrick/compare/carrick-v0.1.12...carrick-v0.1.13) (2026-05-04)


### Features

* drop carrick-org input; cloud derives org from API key ([#83](https://github.com/daveymoores/carrick/issues/83)) ([e133297](https://github.com/daveymoores/carrick/commit/e1332974329dbf34f96a8cd607669469c033fbb2))

## [0.1.12](https://github.com/daveymoores/carrick/compare/carrick-v0.1.11...carrick-v0.1.12) (2026-05-03)


### Features

* split lambdas + LLM prompts to carrick-cloud ([#74](https://github.com/daveymoores/carrick/issues/74)) ([52e1ecb](https://github.com/daveymoores/carrick/commit/52e1ecbd1b571c8925b457ca0f85232a8f176614))

## [0.1.11](https://github.com/daveymoores/carrick/compare/carrick-v0.1.10...carrick-v0.1.11) (2026-05-01)


### Bug Fixes

* **deps:** clear npm Dependabot alerts (lockfile-only) ([#73](https://github.com/daveymoores/carrick/issues/73)) ([35c7656](https://github.com/daveymoores/carrick/commit/35c7656df0a0267e03ec02f921d7252058b39ff7))


### Documentation

* add service map visualization implementation brief ([08c1d02](https://github.com/daveymoores/carrick/commit/08c1d02dcbca9e79365b865c7d774a06540af295))

## [0.1.10](https://github.com/daveymoores/carrick/compare/carrick-v0.1.9...carrick-v0.1.10) (2026-04-20)


### Bug Fixes

* **deps:** clear security audit findings ([#68](https://github.com/daveymoores/carrick/issues/68)) ([37a3d24](https://github.com/daveymoores/carrick/commit/37a3d24b89f9a6fe4e50f2a014a18d812f5928cb))

## [0.1.9](https://github.com/daveymoores/carrick/compare/carrick-v0.1.8...carrick-v0.1.9) (2026-04-20)


### Features

* framework coverage for Koa, Fastify, Hapi, NestJS, Hono ([65a060a](https://github.com/daveymoores/carrick/commit/65a060a39eb1796ef539ea914435fbc7e69f0ce5))


### Documentation

* add growth playbook ([e7ea644](https://github.com/daveymoores/carrick/commit/e7ea644c1e40ec2e995a6305aef31312d2dd9be2))

## [0.1.8](https://github.com/daveymoores/carrick/compare/carrick-v0.1.7...carrick-v0.1.8) (2026-04-09)


### Features

* add --no-cache flag to skip incremental analysis ([#63](https://github.com/daveymoores/carrick/issues/63)) ([f81c53c](https://github.com/daveymoores/carrick/commit/f81c53c63700a274216d0d77aa7f492601929230))

## [0.1.7](https://github.com/daveymoores/carrick/compare/carrick-v0.1.6...carrick-v0.1.7) (2026-04-06)


### Features

* structured logging with tracing, spinners, and S3 log upload ([#60](https://github.com/daveymoores/carrick/issues/60)) ([e56363b](https://github.com/daveymoores/carrick/commit/e56363bd68c6e41c5567e594f547dbfc65576c22))

## [0.1.6](https://github.com/daveymoores/carrick/compare/carrick-v0.1.5...carrick-v0.1.6) (2026-04-06)


### Features

* function intent discovery via LLM-generated descriptions ([#57](https://github.com/daveymoores/carrick/issues/57)) ([97cd3ff](https://github.com/daveymoores/carrick/commit/97cd3ff18941f4b4b2cf0bc33180fe2226bec2b6))

## [0.1.5](https://github.com/daveymoores/carrick/compare/carrick-v0.1.4...carrick-v0.1.5) (2026-04-03)


### Features

* per-endpoint resolved type definitions via compiler ([#51](https://github.com/daveymoores/carrick/issues/51)) ([cd1b8e2](https://github.com/daveymoores/carrick/commit/cd1b8e255fd44fad32146dc8e43b9c42b59e3896))

## [0.1.4](https://github.com/daveymoores/carrick/compare/carrick-v0.1.3...carrick-v0.1.4) (2026-03-29)


### Bug Fixes

* MCP server type safety, bug fixes, and test coverage ([#43](https://github.com/daveymoores/carrick/issues/43)) ([cc80d27](https://github.com/daveymoores/carrick/commit/cc80d27fda58b4946bf38313b84d45dd70e1fa2e))
* remove broken "Test Published Action" workflow ([a50fd70](https://github.com/daveymoores/carrick/commit/a50fd70f0d768652a47df8bc5d60a037421a4168)), closes [#44](https://github.com/daveymoores/carrick/issues/44)
* rename job from "Test Published Action" to "Test Action" ([3de4ef7](https://github.com/daveymoores/carrick/commit/3de4ef79c3335e7ccbf754cf187bf8cce7f15ad2))
* use local action ref and maintain floating v1 tag ([577c872](https://github.com/daveymoores/carrick/commit/577c87295c896b09202932cd084f71ceddf64ab0)), closes [#44](https://github.com/daveymoores/carrick/issues/44)

## [0.1.3](https://github.com/daveymoores/carrick/compare/carrick-v0.1.2...carrick-v0.1.3) (2026-03-23)


### Features

* incremental diff-based analysis with per-file LLM caching ([94b456a](https://github.com/daveymoores/carrick/commit/94b456a6f5930b9b42d1e91dd622d43bc8759565))
* incremental diff-based analysis with per-file LLM caching ([68281dd](https://github.com/daveymoores/carrick/commit/68281dd1984ddacb4a4431c40b9b0577e1bf7edb))


### Bug Fixes

* use repo_path for commit hash and deterministic package.json hashing ([fd792d4](https://github.com/daveymoores/carrick/commit/fd792d466920c440c7228c363ca98aaf4a56772a))
* use std::env::temp_dir() instead of hardcoded /tmp path ([75a7600](https://github.com/daveymoores/carrick/commit/75a7600847a54916547513bc1166bcc2bd17abf7))

## [0.1.2](https://github.com/daveymoores/carrick/compare/carrick-v0.1.1...carrick-v0.1.2) (2026-03-23)


### Features

* add MCP server for AI agent access to cross-repo API data ([1be5314](https://github.com/daveymoores/carrick/commit/1be53144c75862439c716973938267b8f05297b9))
* deploy MCP server as Lambda behind API Gateway ([f5cca06](https://github.com/daveymoores/carrick/commit/f5cca06d23c770201aacf7275ed718cd6f61400c))
* MCP server with Lambda deployment ([0d93f7e](https://github.com/daveymoores/carrick/commit/0d93f7e1ddcf372af5a3b36739c9c6c501df3aba))


### Bug Fixes

* **action:** use GitHub API to resolve release download URL ([e74f22d](https://github.com/daveymoores/carrick/commit/e74f22d9d7250b304bf3416d08da27968c2d7f24))
* address PR review feedback ([63e19e3](https://github.com/daveymoores/carrick/commit/63e19e3e8784c75cbdee488c411874bb2a619c4e))

## [0.1.1](https://github.com/daveymoores/carrick/compare/carrick-v0.1.0...carrick-v0.1.1) (2026-03-08)


### Features

* 3 times retry for gemini call ([3c15b9b](https://github.com/daveymoores/carrick/commit/3c15b9bc31d12059cd9d04949c8cca3912d0ddc6))
* add AST-verified wrapper unwrapping registry ([87f519a](https://github.com/daveymoores/carrick/commit/87f519afe1d79aed1adbd88f95c685bf82338b61))
* add basic request/response comparison ([9debc30](https://github.com/daveymoores/carrick/commit/9debc30918190572db7daf35758b3fa6c45ea498))
* add comprehensive test coverage and testing infrastructure ([6ab65f8](https://github.com/daveymoores/carrick/commit/6ab65f87d6e9d10e75dc904a3a302bae193fac9b))
* add comprehensive test coverage and testing infrastructure ([2e1990a](https://github.com/daveymoores/carrick/commit/2e1990ae74596c55120cea021137adeab0561f9b))
* add config file for identifying env vars and absolute paths ([726ee8e](https://github.com/daveymoores/carrick/commit/726ee8e4d5f653538fcb57fe90de31a309606635))
* add dependency checking ([5e79e7a](https://github.com/daveymoores/carrick/commit/5e79e7a4c8d8a0b570d188769fae3f9450c89d5b))
* add dependency checking ([f3f8cc7](https://github.com/daveymoores/carrick/commit/f3f8cc73b0fa3c54134862cf22bd9593b01660cf))
* add evidence fields to analysis schema ([7fb7576](https://github.com/daveymoores/carrick/commit/7fb75762b9c5555ac1bb67200c6dc928299ce859))
* add evidence to manifests and reports ([e2986fe](https://github.com/daveymoores/carrick/commit/e2986fe24015bd61e9f0dbbac06ec2f11fa2a5d0))
* Add FileAnalyzerAgent for file-centric analysis ([3f939bf](https://github.com/daveymoores/carrick/commit/3f939bf4aedec881672f892395c62c0426cf1d5b))
* Add FileOrchestrator for file-centric processing workflow ([fb288b7](https://github.com/daveymoores/carrick/commit/fb288b7d9076f337a2d26fe59cb875af6150ab7a))
* add github action scaffolding ([6f1cc42](https://github.com/daveymoores/carrick/commit/6f1cc428772141a91464cc94e7dee24fcc54175f))
* add json enum type ([88e5c0f](https://github.com/daveymoores/carrick/commit/88e5c0fa858ba6fdf7b4afc40eaa29d11e12d6b6))
* add lambda client ([d74a7bb](https://github.com/daveymoores/carrick/commit/d74a7bb70a360a922f8fdfa39c9c641848b47170))
* Add mock response generation for file analysis schema ([6ee3d33](https://github.com/daveymoores/carrick/commit/6ee3d337d61a6f5152b77f608582203065028863))
* add prompt for agent ([33f5f05](https://github.com/daveymoores/carrick/commit/33f5f051e3791ba0faaf9995d64a7751e45443f4))
* Add response type extraction to file-centric analysis ([5fbc40b](https://github.com/daveymoores/carrick/commit/5fbc40bab27352598c4a68c73fa4e29c4774ba43))
* add stable candidate ids and spans ([2a428c5](https://github.com/daveymoores/carrick/commit/2a428c5e38218d8293e9aa160dbab34411de2ac5))
* Add SWC Scanner (AST Gatekeeper) for file-centric analysis ([49e3462](https://github.com/daveymoores/carrick/commit/49e3462975a77f52c216c732a7335b3aaab90b4b))
* add URL normalization for cross-service endpoint matching (P0/P1 complete) ([15a42d1](https://github.com/daveymoores/carrick/commit/15a42d1b67c46c191b96aaf73b5d7de0a1b7be12))
* adding gemini lambda ([d6e2058](https://github.com/daveymoores/carrick/commit/d6e2058c3c132a4c776ec2b976f5c58876740bc7))
* adding gemini lambda ([fdac28a](https://github.com/daveymoores/carrick/commit/fdac28aae7202da2cad5e6020ad8f2b014132518))
* adding gemini service for service calls ([1121322](https://github.com/daveymoores/carrick/commit/11213224300502428b3bbf78430fef240c1d156c))
* adding more complex service definitions ([8a22a03](https://github.com/daveymoores/carrick/commit/8a22a03b24d15112c5ffe46a9ba25e4d261f0cdf))
* adding more realistic services and calls ([520c76e](https://github.com/daveymoores/carrick/commit/520c76e98dd560464a84819dc1ee559b23a1fe4b))
* adding research document ([3bf4d34](https://github.com/daveymoores/carrick/commit/3bf4d3489f776e2a69d4ede2a0cd65f459b05ffb))
* adding response types for inline functions ([aece321](https://github.com/daveymoores/carrick/commit/aece3217a1b46be901a420019c86c07ce6a0805d))
* align manifest schema and type pipeline ([5f52ef9](https://github.com/daveymoores/carrick/commit/5f52ef9c64402d2d70398fdc1160efdcef2b7a6c))
* allow nested app and routers ([978d779](https://github.com/daveymoores/carrick/commit/978d77979555623958b8b246ae3e68d3225bea49))
* allow router and app nested use calls ([963d9cc](https://github.com/daveymoores/carrick/commit/963d9cc11840afba5a73718fe2b33a27970fdc09))
* allow usage of env vars in template strings and definitions outside of the strings ([2b9c418](https://github.com/daveymoores/carrick/commit/2b9c418dd62d6461a434eca35045e960ed8ac45b))
* analyze response and requests ([3caf58b](https://github.com/daveymoores/carrick/commit/3caf58bbf6a1424ef5b928b9ce490b19267ba130))
* better output ([81a6ebf](https://github.com/daveymoores/carrick/commit/81a6ebff52a3b0ae12bb07547803ae6e77b06532))
* better output ([2448aab](https://github.com/daveymoores/carrick/commit/2448aabd23776b137ea2367ad73a7018885da369))
* check for calls to endpoints with :params ([8a16a8b](https://github.com/daveymoores/carrick/commit/8a16a8babfaf9ef90ed0820572176ae52189665f))
* cloud storage ([4754c68](https://github.com/daveymoores/carrick/commit/4754c687743c5f98c13e1c9296941b71c4bbcae4))
* cloud storage ([613aabd](https://github.com/daveymoores/carrick/commit/613aabdd5d86862445c7690959ec12212853042f))
* correct route matching ([c0b3985](https://github.com/daveymoores/carrick/commit/c0b39853d409fb1a2ccbd0a4db5f90990206d916))
* create full type files ([73894dc](https://github.com/daveymoores/carrick/commit/73894dc7d374c88e6d4ac84389a286430123ca7f))
* create package.json for temp type checking ([390b322](https://github.com/daveymoores/carrick/commit/390b32267b30664e3970b5a4968ba51b160983af))
* creating a minimal tsconfig for type checking in CI runs ([7c3e1cd](https://github.com/daveymoores/carrick/commit/7c3e1cda880075c2c14a1b6286b542a082a7a60e))
* dedupe cals to prevent duplicate warnings ([2fdfd27](https://github.com/daveymoores/carrick/commit/2fdfd276054f0cc73574b0978b89a4cfee29f1ea))
* default tsconfig ([652c963](https://github.com/daveymoores/carrick/commit/652c963703a9754d8192130398ee07e5cab2e2f8))
* define ApiEndpointDetails ([57ca7a8](https://github.com/daveymoores/carrick/commit/57ca7a8b848e9f19e20328db44eece57e222a11e))
* design Compiler Sidecar architecture for robust type extraction ([1f52f9c](https://github.com/daveymoores/carrick/commit/1f52f9cd86c9cc03279a99b858d4207088b497f7))
* different type extraction ([9d6c95e](https://github.com/daveymoores/carrick/commit/9d6c95e0f88257f266ee528a77fc84dd78b7f7af))
* enhance mock mode with intelligent response generation ([df28aff](https://github.com/daveymoores/carrick/commit/df28aff381698da05ef7ddff191426ad3f31d400))
* extract and parse type information ([e8715e9](https://github.com/daveymoores/carrick/commit/e8715e97b2e269107af2d590006be40e1dcdb87c))
* extracting types ([5a0f572](https://github.com/daveymoores/carrick/commit/5a0f5723b5e49dea66cde67f1e9beca5de9d91ea))
* find dynamic route paths ([dffe253](https://github.com/daveymoores/carrick/commit/dffe25361551c04fd932ff412c9c608322b6b923))
* find dynamic route paths ([3348e11](https://github.com/daveymoores/carrick/commit/3348e115817e1dc6f2aaa3eba9834772534cd8a0))
* fix formatting ([8dba3ea](https://github.com/daveymoores/carrick/commit/8dba3ea996a86f721d826a9594d6c973c29399c8))
* fix issues after rebase ([7df2ddc](https://github.com/daveymoores/carrick/commit/7df2ddc7ab52d34abc90842b1c862e93cbb76ed9))
* fix matchit matching ([f745a1f](https://github.com/daveymoores/carrick/commit/f745a1f0cd7a87ffca7404f3e365ccadccb01397))
* fix proxy ([99d667e](https://github.com/daveymoores/carrick/commit/99d667e270d806898b30207627427a3890316e59))
* fixing warnings ([7ea8193](https://github.com/daveymoores/carrick/commit/7ea819349cded12485d1b8fae9a4b31249478691))
* formatting ([b9474cb](https://github.com/daveymoores/carrick/commit/b9474cbc567df793bec390a42d574d5d685faee6))
* function parsing and response body reading ([d513e1f](https://github.com/daveymoores/carrick/commit/d513e1f0127ede8192009f6a43d7229dba46a756))
* generating unique type files in CI mode ([1f7515d](https://github.com/daveymoores/carrick/commit/1f7515d110a145687dd062341e8e9613f5b10d43))
* get request and response type into analyser ([2f890cc](https://github.com/daveymoores/carrick/commit/2f890cc7011abdb9a3d0d603e5dc97af0eef1e39))
* getting correctly prefixed output type files ([28c818e](https://github.com/daveymoores/carrick/commit/28c818e71911a9c66b2d437ccfb8b6cb1d8074b6))
* getting repo names for type output file name ([a749f00](https://github.com/daveymoores/carrick/commit/a749f0057e583ba3f52f1785f5007214d2b13277))
* hard code gemini model variant ([5d51542](https://github.com/daveymoores/carrick/commit/5d515429707913787e08401d267694ef53afd919))
* implement fetch-to-json call correlation for consumer type matching ([d664582](https://github.com/daveymoores/carrick/commit/d6645826740ee9f76e204e1894b839536a47b9a1))
* improve env var classification guidance for cross-repo matching ([42ddcc2](https://github.com/daveymoores/carrick/commit/42ddcc2a08cb51eb2b09b5a03c36b52eb7e5757b))
* improve llm extraction ([40da714](https://github.com/daveymoores/carrick/commit/40da714da889c36610cefa768a4f9a6f76627141))
* improving prompt to extract the types more accurately ([1169965](https://github.com/daveymoores/carrick/commit/116996555fde0579c643ef568c1432fdf5b4fa08))
* include call-chain context in SWC candidate hints ([530e831](https://github.com/daveymoores/carrick/commit/530e831d5f47cbac1d9bf337784f30539c344de2))
* increment consumer types to prevent duplicates ([13c4c43](https://github.com/daveymoores/carrick/commit/13c4c438bcfa22c524e7dd0ad66be6d0ec3f1cae))
* Integrate SWC Scanner as Gatekeeper in FileOrchestrator ([78ba73b](https://github.com/daveymoores/carrick/commit/78ba73b244ccf7f1ada541aaa61981a2cd52131c))
* make action ref v1 ([fb2aa7a](https://github.com/daveymoores/carrick/commit/fb2aa7ab24d96e7cad291ebba4d136699e6933ba))
* make sure :params matches dynamic strings ([0675481](https://github.com/daveymoores/carrick/commit/0675481a76d112d8f2b0fcd97d6c6f8c69f17ec0))
* make typecheck output less brittle ([1455a3d](https://github.com/daveymoores/carrick/commit/1455a3d4e30ac2167707a06ab582e4f1a8a3285d))
* making test-repo a real project ([5eec320](https://github.com/daveymoores/carrick/commit/5eec3204fce23cbb60b86b97334b8eef177a7490))
* move to span-based type inference requests ([0184680](https://github.com/daveymoores/carrick/commit/0184680643e85dfa091e5c2357b2364774afa886))
* move type check into seperate script at the end ([f8feee1](https://github.com/daveymoores/carrick/commit/f8feee1936990d99512b9ec3f6f5bdb1dc2917fd))
* moving shared functionality into extractor trait and adding analyzer struct ([316100d](https://github.com/daveymoores/carrick/commit/316100dba81cb9023d6cd1bc3c3979ae72516795))
* moving to single lambda ([ee67452](https://github.com/daveymoores/carrick/commit/ee674526d18d94d788c8638100ad99b009b48ca2))
* Multi-Agent Framework-Agnostic Analysis System ([ac18a36](https://github.com/daveymoores/carrick/commit/ac18a363af8ad9e8b9b499ce28e17d3884b514b0))
* omit some type errors ([cae2016](https://github.com/daveymoores/carrick/commit/cae2016bc3e7175679e1b822f5b37cc8bc6c06f9))
* only upload on main ([01ce3ed](https://github.com/daveymoores/carrick/commit/01ce3ed6195228b94a2f6b71a290cbeaef6d1d3a))
* output action ([da16ce5](https://github.com/daveymoores/carrick/commit/da16ce5012ac2b61e4d687c5b038957a53fedb44))
* output action ([bb33bb6](https://github.com/daveymoores/carrick/commit/bb33bb6a04a5c2891584b4d5dcbc62abaab2a413))
* pass structured candidate context to analyzer ([f6225aa](https://github.com/daveymoores/carrick/commit/f6225aaf342f56125d37c3ff64eb62c18f8a1c1b))
* **phase0:** add comprehensive debug logging and validate multi-agent system ([c7cdbc7](https://github.com/daveymoores/carrick/commit/c7cdbc727a0e72122988937a8aff3ce34cb395c8))
* populate config accurately ([d84dfea](https://github.com/daveymoores/carrick/commit/d84dfea625b6c1ba314ef8dc35790ec40608077e))
* prefix routers and apps with repo name ([e29cd7a](https://github.com/daveymoores/carrick/commit/e29cd7af964bd79ba5b335ade93558a8518acece))
* prevent duplicate routes and type aliases ([78d97ea](https://github.com/daveymoores/carrick/commit/78d97ea291c28fc36afb43bd3c758a3dcef05306))
* prevent duplicate type names ([f1007ad](https://github.com/daveymoores/carrick/commit/f1007adfad36d5842b408559bc21747b0441db1a))
* prevent duplicates for env ([e7cf173](https://github.com/daveymoores/carrick/commit/e7cf1739db0c7ce794378a9101a56826e9055d3d))
* prevent gemini call when pulling metadata from aws ([aa3cf67](https://github.com/daveymoores/carrick/commit/aa3cf67b6b7af3d6750cf42dac8759e5dc4f9b1a))
* prevent race condition with s3 downloads ([1a85f57](https://github.com/daveymoores/carrick/commit/1a85f57c3854c10325ea0695c536c34f5f6d1b29))
* prevent removal of packages file too early ([5c94208](https://github.com/daveymoores/carrick/commit/5c942080315a7249cfe56d328ae306c8181527b9))
* process missing types of type nodes ([01d9edb](https://github.com/daveymoores/carrick/commit/01d9edb8c8627f48e16b5ad8c879083e7ab2b2fc))
* recurrsively find fetch statements ([9e1ffc7](https://github.com/daveymoores/carrick/commit/9e1ffc791d479ac94c4943a9c6731dc1ebcdbc25))
* reduce delay between triage calls and add terraform instructions into warp md ([8ac63b1](https://github.com/daveymoores/carrick/commit/8ac63b1d5b482d4d2f6f8758131198db1de9eb00))
* refactor of type extraction ([e752b52](https://github.com/daveymoores/carrick/commit/e752b5225aa94be710dba2d5fb1d8219200f7e40))
* refactoring ([7c8328e](https://github.com/daveymoores/carrick/commit/7c8328eb3528f4339ce53c759140d0a58cd2fd51))
* refactoring ci_mode ([2547382](https://github.com/daveymoores/carrick/commit/25473824d422a0d5054df4147b135910d1e68bc1))
* remove output directory ([3c79f4e](https://github.com/daveymoores/carrick/commit/3c79f4e8e24f47a75b3eb0e2b888708c9e4df6bf))
* remove specific type names ([a10b7f5](https://github.com/daveymoores/carrick/commit/a10b7f5415288718121ea03cfec5e4985b02dbd2))
* remove specific type names ([6272f44](https://github.com/daveymoores/carrick/commit/6272f442666c36e47d711d64a9f745171acc16ab))
* remove thinking budget ([5cd3544](https://github.com/daveymoores/carrick/commit/5cd3544b287a97da520173cf77f9374268078edb))
* removing node_modules ([416cd49](https://github.com/daveymoores/carrick/commit/416cd49eb96310f5247a5b6a1989fe930a56d5df))
* report orphaned endpoints ([ab47cd3](https://github.com/daveymoores/carrick/commit/ab47cd3657a6d47b9e188927a493b343ad84a281))
* resolve nested router paths ([1632c1b](https://github.com/daveymoores/carrick/commit/1632c1b28a0248d5eae229899e5a72092b6b8e7d))
* resolving types for fetch and json calls ([78af106](https://github.com/daveymoores/carrick/commit/78af106c2f54a67e35a1cd43e78e4eb448c13122))
* **sidecar:** add deterministic def-use inference for call results ([c0e0918](https://github.com/daveymoores/carrick/commit/c0e09182a519bba6bbbdb0d8341229917a1ed342))
* **sidecar:** Implement Phase 1 - Node.js Type Sidecar ([fb3f06f](https://github.com/daveymoores/carrick/commit/fb3f06f864a77548469b1138c02dcde25c8f602e))
* **sidecar:** Implement Phase 2 - Rust Integration ([c5fd1fa](https://github.com/daveymoores/carrick/commit/c5fd1faf43a71e59190e744e9ba45d16f1886b45))
* **sidecar:** infer request bodies and bundle modules ([4a8ea98](https://github.com/daveymoores/carrick/commit/4a8ea98b2f90d821f2160cf44e6ece3be1cfd8a2))
* **sidecar:** infer types by span instead of line windows ([8cf202d](https://github.com/daveymoores/carrick/commit/8cf202d0abc21f117887dcddc0445183a25ad6c5))
* strip query params when doing path matching between calls and endpoints ([3834e08](https://github.com/daveymoores/carrick/commit/3834e0801cb4f19bf71d7416feccee632b7a8992))
* terraform ([b28f528](https://github.com/daveymoores/carrick/commit/b28f528d4afc9bd2dd09de75d2ca27ea847ae989))
* test for express routers ([3b7064e](https://github.com/daveymoores/carrick/commit/3b7064e04403d9d1ff1c2fb625e8c63429f67b49))
* track different types of function exports ([a74bc92](https://github.com/daveymoores/carrick/commit/a74bc92db6f631e196196129141e44cd889de6b5))
* trying to fix path bug ([664a4f0](https://github.com/daveymoores/carrick/commit/664a4f00919b6526b42d37d4477565da5b4c882d))
* trying to get and resolve type information for request response types ([58b0cf9](https://github.com/daveymoores/carrick/commit/58b0cf999afb004e4ffe5dfe66331fa842f096e5))
* trying to get single lambda to work ([8239307](https://github.com/daveymoores/carrick/commit/8239307ee1845a4678537236adc884ae5f1a06f3))
* trying to match up routers ([14be798](https://github.com/daveymoores/carrick/commit/14be798397ea5a3b67c75f17ae8da3a19024076e))
* type compat check ([72a2c48](https://github.com/daveymoores/carrick/commit/72a2c4875a3eec17710cc94f9c8b2803be704f12))
* **type-checker:** Implement Phase 3 - Type Checker Refactor ([7d0e49c](https://github.com/daveymoores/carrick/commit/7d0e49cc1786e4635450cc6e3c4a105fe87b943f))
* unique response names ([e282f62](https://github.com/daveymoores/carrick/commit/e282f627a4c775970219b1f8114de426ff20d7c8))
* update prompt for more accuracy ([f6a9ada](https://github.com/daveymoores/carrick/commit/f6a9adafb2078d0758b5093b6b86a83f2f5cb3ea))
* update tests for output ([9305f9e](https://github.com/daveymoores/carrick/commit/9305f9e007e3dc4160d1c83e7acb4936ff8e2025))
* uploading individual repo data ([5778c60](https://github.com/daveymoores/carrick/commit/5778c60e809a87252cc8052cc60f52e1674378fb))
* uploading individual repo data correctly but without type files in doc storage ([9c47f92](https://github.com/daveymoores/carrick/commit/9c47f92e064974bd8329b451599a60a483fd4f23))
* using matchit for path matching ([f0b14ae](https://github.com/daveymoores/carrick/commit/f0b14ae3d575b08c6749d2e79b29f76622998e55))
* using matchit for path matching ([bfa5343](https://github.com/daveymoores/carrick/commit/bfa53437610680fb68d1f614540f12048ad39665))
* working lambda ([e8a6dc5](https://github.com/daveymoores/carrick/commit/e8a6dc51f4ed1ab4d20b44df4028d95f5c71ad79))
* working on extracting types from position ([f45fd8c](https://github.com/daveymoores/carrick/commit/f45fd8c54b1f9006945d5e786bc2f8c4b6c69cd9))


### Bug Fixes

* add directory name if not in CI run and improve prompt ([4c23836](https://github.com/daveymoores/carrick/commit/4c23836c7273a8f1ac3fdcfc05cb193288f4b44a))
* Add fallback type search when LLM position is incorrect ([d8f0e92](https://github.com/daveymoores/carrick/commit/d8f0e92add09fdfa5633a1eed756abfd993766c3))
* add logging ([47c75f2](https://github.com/daveymoores/carrick/commit/47c75f251bd7b7eade3523271d82c628651c0e26))
* address all clippy linting warnings and add fmt/clippy to pre-commit hook ([ac34e0f](https://github.com/daveymoores/carrick/commit/ac34e0f1ed8c8a36488e13afd625b536233449ad))
* address Copilot PR review feedback ([be4de85](https://github.com/daveymoores/carrick/commit/be4de85bcdc14f33d864c3f75b3a123221e534e7))
* allow lamdas to paginate dynamo db data ([f56db9d](https://github.com/daveymoores/carrick/commit/f56db9d6ba0a3a74ff70db39cc0a8f67a098c972))
* avoid Response wrappers in type aliases ([dba7f7d](https://github.com/daveymoores/carrick/commit/dba7f7dac9f9d45e9955a49a240e2e188105bc5b))
* **ci:** Node 22 LTS, mock API key, bytes crate vulnerability ([18029f2](https://github.com/daveymoores/carrick/commit/18029f22a5855a3a3a3269803a46b87d5fcaa34b))
* **ci:** sidecar archive path, caching, and consistency fixes ([c68153f](https://github.com/daveymoores/carrick/commit/c68153f1bd582e71d59c659c7d09f1aa7a287256))
* **ci:** update regression test to expect 3 endpoints ([328899a](https://github.com/daveymoores/carrick/commit/328899a9504410481271f83a12d79a1129735948))
* correctly traverse router app relationships ([72e7316](https://github.com/daveymoores/carrick/commit/72e731632fae44ea9bfe80199cf1d2b441645abc))
* dedupe type symbol requests ([de4d8aa](https://github.com/daveymoores/carrick/commit/de4d8aa2cf456149f2808b36fb1b5e70fca6f453))
* duplicate path issue ([e399e50](https://github.com/daveymoores/carrick/commit/e399e5077722024d8378aa8094adda97371bbaa9))
* fix CI regression tests ([7f7b027](https://github.com/daveymoores/carrick/commit/7f7b027877486a1372b24c4053515a0fc151e539))
* fix dependency resolution issue ([4ce7bec](https://github.com/daveymoores/carrick/commit/4ce7bec88dda7e9acfdf71f3eae18be5d29b25c9))
* fix merging of configs ([d29936d](https://github.com/daveymoores/carrick/commit/d29936d70ea213e379ff8ab986bbd44b510053c9))
* fix routing issue ([62e4d90](https://github.com/daveymoores/carrick/commit/62e4d9080554c84d870f6d677bf17c40dd73f834))
* fix s3 file download ([5133b52](https://github.com/daveymoores/carrick/commit/5133b52cf01b08f51c855c61ca28eb7f04cb62b9))
* get repo name in CI ([0a0ad9b](https://github.com/daveymoores/carrick/commit/0a0ad9b81776cad00334830fc4cbfc2517a227c3))
* handle template literal path params in alias generation ([da0221f](https://github.com/daveymoores/carrick/commit/da0221f4611b1b32bb41c09c0430f4164ba9fe06))
* human-readable type names in output, filter non-HTTP endpoint methods ([7038ca2](https://github.com/daveymoores/carrick/commit/7038ca2757552fa63ecfb587cee585008f8f3f84))
* improve cross-repo type matching ([ed3a523](https://github.com/daveymoores/carrick/commit/ed3a523a8b519d9500f48687a011d04053736b05))
* install deps correctly ([965c5a1](https://github.com/daveymoores/carrick/commit/965c5a1f15db41f5ee4a14e5a1922d84e400b142))
* missing gemini key from release ([4d1fe49](https://github.com/daveymoores/carrick/commit/4d1fe496707f446eb47384ad5e147315601cd6d3))
* **mount_graph:** fix alias detection to use exact location match ([8337196](https://github.com/daveymoores/carrick/commit/8337196c40a14171acd348202590eee4a0086c81))
* **mount_graph:** resolve nested router path resolution with name aliases ([a09378b](https://github.com/daveymoores/carrick/commit/a09378ba26d758bbc405d78117d4c6d97a687a71))
* normalize template literal path params to :param style ([810d6d3](https://github.com/daveymoores/carrick/commit/810d6d36a1f80c166fafc204c467b61a4600b4f2))
* **normalizer:** strip backticks leaking from template literals into manifest paths ([ae12c61](https://github.com/daveymoores/carrick/commit/ae12c612488ae91e7ea1400a83c94098257b8405))
* per-file SWC spans, text-based inference fallback, schema cleanup ([5505187](https://github.com/daveymoores/carrick/commit/5505187b84b5753795bcd232c16e0ba5d6d0c146))
* remove warnings ([342c646](https://github.com/daveymoores/carrick/commit/342c64667c62b84b3f240acd6abfd098bf10c174))
* resolve rebase conflicts - remove duplicate MockStorage::new and unused RouteFieldsMap ([f6d434b](https://github.com/daveymoores/carrick/commit/f6d434bbc46f84abd17be37963c12b336e4c50bc))
* routing fix ([feab6d3](https://github.com/daveymoores/carrick/commit/feab6d3f62d349adcba3048f38df2678c823a1cc))
* Sanitize LLM response and use correct startPosition field name ([9d36b71](https://github.com/daveymoores/carrick/commit/9d36b71b36dd2d697c29bb4b0c06294df5871adf))
* **sidecar:** prefer json payload types ([45179f1](https://github.com/daveymoores/carrick/commit/45179f1b9770bef14b5f8f4a0df5d1bdefc23f37))
* **sidecar:** wrong moduleResolution default, false-compatible on tsc failure ([8e86834](https://github.com/daveymoores/carrick/commit/8e86834e2a8eca9a5b3ee95babf7721da92e5304))
* **ts_check:** add @types/node for type-checking script ([4cbc333](https://github.com/daveymoores/carrick/commit/4cbc333bfe9e5d75d37a13b3a401bfdc7f32a6b7))
* **ts_check:** compare unknown types when resolved ([98a63ad](https://github.com/daveymoores/carrick/commit/98a63ad541e21e77f208d7b126b97100ed2c29ae))
* Use SWC AST to compute accurate type positions from line numbers ([0d6464c](https://github.com/daveymoores/carrick/commit/0d6464cbda6203d8396e998a38ae117210de6b51))
* validate type symbols against real imports ([f53f40b](https://github.com/daveymoores/carrick/commit/f53f40be627de9751cb150515698ea592fb4f8e1))


### Refactoring

* add import context to file analyzer prompts ([8730250](https://github.com/daveymoores/carrick/commit/873025075c59e5bdab2d10306c7806cf0df3351f))
* AST-gated file-centric analysis with TypeScript compiler sidecar ([b41ff62](https://github.com/daveymoores/carrick/commit/b41ff6212593487cd7a557a472041004f3376f13))
* file-centric gemini analysis ([4381c3f](https://github.com/daveymoores/carrick/commit/4381c3ff976e7fb29e5fae7ce44a43a6be0b7223))
* Gemini 3 upgrade, manifest validation, route-aware matching ([d8cdcc3](https://github.com/daveymoores/carrick/commit/d8cdcc3b7611afd8e572e390691f28962694c5d3))
* **orchestrator:** SWC span fallback + framework-aware type scrubbing ([8bb543e](https://github.com/daveymoores/carrick/commit/8bb543ed41b8cdb79c42a286903bbddb8337016d))
* **phase4.1:** archive legacy TypeScript type extraction code ([4821432](https://github.com/daveymoores/carrick/commit/48214328c0e97f2ffd4154eb08f4870194b9fbfa))
* **phase4.1:** remove legacy type position code from Rust ([2032a88](https://github.com/daveymoores/carrick/commit/2032a88477e36345b628a2382dfb94ffe41efd21))
* remove dead JSON type comparison code (P2 complete) ([ea06593](https://github.com/daveymoores/carrick/commit/ea06593c47fa3fd2c1e8b36f6f6b6f34b076a49e))
* remove DependencyVisitor (570 lines of dead code) ([a1b36e0](https://github.com/daveymoores/carrick/commit/a1b36e0f463c0f9855daae1a701b158940b21a38))
* remove legacy extraction and normalize URLs ([1669e32](https://github.com/daveymoores/carrick/commit/1669e32f2fa9f2b6a8b48a88763f367b73a9da80))
* Remove old Batch-of-10 orchestration, use AST-Gated File-Centric approach ([8fd8f58](https://github.com/daveymoores/carrick/commit/8fd8f58456c588b93189edee5f46cbff226878e3))
* replace DependencyVisitor with lightweight ImportSymbolExtractor (P4 complete) ([1a62355](https://github.com/daveymoores/carrick/commit/1a623559948b20c119a0170914fe0cfb1cd338a1))
* replace Gemini byte-offset spans with expression text + line number ([5ea222d](https://github.com/daveymoores/carrick/commit/5ea222d91761a19ca7608f226ee34ec6abdcf938))
* **sidecar:** synthetic monorepo stub snapshot ([6a8536d](https://github.com/daveymoores/carrick/commit/6a8536de5d0d1dc50f2a6e0b6a69ab8bdf9cea0d))
* **type-checker:** Remove legacy mode - manifest-only type checking ([8e33cf9](https://github.com/daveymoores/carrick/commit/8e33cf986337de1a174a21c4ede204c59a4fca06))
* wire sidecar type resolution ([f19ab8a](https://github.com/daveymoores/carrick/commit/f19ab8aafdc19fdd95850c1de75e89e293a5d3ab))


### Performance

* **framework-guidance:** flatten JSON schemas for parallel LLM execution ([e822daa](https://github.com/daveymoores/carrick/commit/e822daad69cda8ac3985d61b0cbb7e43c77be4e4))


### Documentation

* add analysis of persistent bugs and architectural gaps ([9cd4b49](https://github.com/daveymoores/carrick/commit/9cd4b49f241042fea029882b0eb13000db3ab28c))
* Add architecture documentation for file-centric analysis ([e31d413](https://github.com/daveymoores/carrick/commit/e31d413bc5b93d84b1753b39928f8e14d870b81c))
* add compiler sidecar completion phases ([98bd0cd](https://github.com/daveymoores/carrick/commit/98bd0cd86f913e143e998b252d8a993c876369fc))
* add comprehensive context for remaining issues 4 and 5 ([9040bef](https://github.com/daveymoores/carrick/commit/9040befe1541a4cf08058273a1e9d5f87f5bba0f))
* add contributor guidelines for agents ([eee5932](https://github.com/daveymoores/carrick/commit/eee5932cc5c88ded4e33083736c8e4fddac3924b))
* Add critical warning about not hybridizing analysis flows ([38a4b7f](https://github.com/daveymoores/carrick/commit/38a4b7f03d17a5a03aa40b75cac3f54158ac9ad6))
* add next-steps research documents ([49226ee](https://github.com/daveymoores/carrick/commit/49226ee38098291907166f91778e85dd01243ea1))
* add single-file slicing plan ([62f1eb7](https://github.com/daveymoores/carrick/commit/62f1eb754b5083f1bd7600b8ee805eb4ef79dc5a))
* clarify env-var config suggestion issue ([52780b9](https://github.com/daveymoores/carrick/commit/52780b93a38e37067cd6ce46ca8f12589ffc1bf9))
* clarify legacy code removal ([97afe89](https://github.com/daveymoores/carrick/commit/97afe89e80ef959fe7655d8087eb00f4f39c9bed))
* document context-first type checking flow ([d4ebe20](https://github.com/daveymoores/carrick/commit/d4ebe20f5b131c35d1c4e0c75f2c13fac79f22a2))
* explain manifest alias resolution gaps ([40e0ee3](https://github.com/daveymoores/carrick/commit/40e0ee38d65a2c4f4cc41d620d8f64410e10bd03))
* improving logging ([a1efd0e](https://github.com/daveymoores/carrick/commit/a1efd0e04d9ca1e21222db657942ecf2e5e99919))
* **phase4.2:** update documentation for compiler sidecar architecture ([1e6a65b](https://github.com/daveymoores/carrick/commit/1e6a65b77b3eba062a4f3ae73666cca8d607d0b2))
* refine Compiler Sidecar architecture based on practical concerns ([acd79c9](https://github.com/daveymoores/carrick/commit/acd79c9a814a97adca990729797ae122713d2727))
* remove date from signup ([5bf6b16](https://github.com/daveymoores/carrick/commit/5bf6b163218ac3958af5b14832bac2029bf95f5a))
* remove heading from example ([931535c](https://github.com/daveymoores/carrick/commit/931535cad88039cd2307329fa53394669306a4cb))
* remove summary files ([8d255b2](https://github.com/daveymoores/carrick/commit/8d255b27e780dc6bf09b07cf9e1d4126709683f7))
* restore research documentation, tests, and analysis from main branch ([71fc90f](https://github.com/daveymoores/carrick/commit/71fc90fbf7e80011586650f3c5ee1646733b1762))
* simplify .thoughts top-level ([0baa952](https://github.com/daveymoores/carrick/commit/0baa952f5cabaf9c15d7ab29f3244e25bfbebb15))
* typo ([9327468](https://github.com/daveymoores/carrick/commit/9327468cf5f60f4f022fa1a267593a8adeac9056))
* update analysis report with mount graph fix status ([09c8ff2](https://github.com/daveymoores/carrick/commit/09c8ff26477e48f1aed8fa0e04126d3186e51246))
* update comment ([64c01a5](https://github.com/daveymoores/carrick/commit/64c01a5bd493de2be327b3fb130a5ff2d5a71303))
* update readme ([dfeed48](https://github.com/daveymoores/carrick/commit/dfeed48844e9f5f93bec0dee8f6a55173267a4fd))
* update readme ([6767cca](https://github.com/daveymoores/carrick/commit/6767ccab190610c4df312be5b8f140b6c7265af2))
* update readme ([0f426de](https://github.com/daveymoores/carrick/commit/0f426de281ab6145204ad59fa70066812d120dc0))
* update readme ([1267cd8](https://github.com/daveymoores/carrick/commit/1267cd8ab6cac1b6aa302f068482a5e6cb1a3213))
* update readme ([4b685e1](https://github.com/daveymoores/carrick/commit/4b685e1324e4b3ab2db4b69d4c94141a8a290e76))
* update readme with correct installation instructions ([57d1927](https://github.com/daveymoores/carrick/commit/57d1927bcec342e94ad311586a795b60f56b913c))
* update remaining_issues_analysis with current state of Issue 7 ([757667d](https://github.com/daveymoores/carrick/commit/757667dabe47aba8fb760d42cbe48129c050af1d))


### CI/CD

* fix broken test refs, bundle sidecar, add release-please ([a398965](https://github.com/daveymoores/carrick/commit/a398965226211158d2e47acdee507a698f8c0517))
* remove unnecessary ANTHROPIC_API_KEY - tests run in mock mode ([9367c1d](https://github.com/daveymoores/carrick/commit/9367c1d66ef519ae11d4c50d9b9992f08e5737e0))
* update workflow for new test structure ([34864f4](https://github.com/daveymoores/carrick/commit/34864f4b6a06c141873d7b0158993582634798aa))

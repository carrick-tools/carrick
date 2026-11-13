# model-row-runtime-base

`src/client.ts:6` reads its base from a call (`apiBase()`), so the site pass stops on its URL, and the cassette holds the model's bare path `/things/list`. That row states no base (carrick#4171). Line 11 is the template twin (injected base) and line 16 a plain bare path (relative). The test is `tests/model_row_runtime_base_test.rs`.

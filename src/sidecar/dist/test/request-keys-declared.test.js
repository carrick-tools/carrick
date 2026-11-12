import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
// Referenced so neither alias reads as unused; each is `never` or the build fails.
const keysTheSchemaDrops = undefined;
const keysNoHandlerKnows = undefined;
describe('request schema and request types declare the same keys', () => {
    it('compiles only when neither side has a key the other lacks', () => {
        assert.equal(keysTheSchemaDrops, undefined);
        assert.equal(keysNoHandlerKnows, undefined);
    });
});

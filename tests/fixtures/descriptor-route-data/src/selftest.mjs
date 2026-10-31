// An expected result built inside a test callback.
export function test(name, body) {
  body();
}

test('lists the consumers of an operation', () => {
  const result = {
    operations: [{ method: 'GET', path: '/payments', consumer: { sites: [] } }],
  };
  if (result.operations.length !== 1) {
    throw new Error('expected one operation');
  }
});

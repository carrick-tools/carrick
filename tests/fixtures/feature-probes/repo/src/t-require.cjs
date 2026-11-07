const { http } = require('@fixture/http');
function f() { return http.post('/p/t-require', null); }
module.exports = { f };

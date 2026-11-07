export async function f() { const { http } = await import('@fixture/http'); return http.post('/p/t-dynamic-import', null); }

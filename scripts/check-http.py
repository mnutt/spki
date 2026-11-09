#!/usr/bin/env python3
"""Exercise worker HTTP metadata through the actual host and WebSession adapter."""
import json, os, pathlib, subprocess, sys, tempfile
client = ['/opt/app/target/debug/grain-smoke', sys.argv[1]]
def request(path, body=None, mode='web', **options):
    env = dict(os.environ, **options)
    with tempfile.NamedTemporaryFile(mode='w') as file:
        if body is not None:
            file.write(body); file.flush()
        result = subprocess.run(client + [path, file.name if body is not None else '-', mode], env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (path, result.stdout, result.stderr)
    return result.stdout.strip()
def status(): return json.loads(request('__spki/v1/status', mode='api'))['revision']
source = '''export default { async fetch(request) {
 const url = new URL(request.url);
 if (url.pathname === '/form') return Response.json(Object.fromEntries(await request.formData()));
 if (url.pathname === '/echo') return Response.json({method:request.method, body:await request.text(), headers:Object.fromEntries(request.headers)});
 const status = Number(url.pathname.slice(1)) || 200;
 return new Response([204,205,304].includes(status) ? null : 'body', {status, headers: {
  'content-type':'text/plain', 'location':'/next', 'etag':'"version"',
  'set-cookie':'session=abc; Path=/; HttpOnly; Max-Age=60', 'x-sandstorm-app-test':'retained'
 }});
}};'''
request('__spki/v1/activate', json.dumps({'source':source, 'expectedRevision':status()}), 'api')
for method in ['GET', 'POST', 'PUT', 'PATCH', 'DELETE']:
    result = json.loads(request('echo', 'payload' if method in ['POST', 'PUT', 'PATCH'] else None,
        SPKI_SMOKE_METHOD=method, SPKI_SMOKE_MIME='text/plain', SPKI_SMOKE_ENCODING='test-encoding',
        SPKI_SMOKE_IDENTITY='1', SPKI_SMOKE_COOKIE='1',
        SPKI_SMOKE_HEADERS=json.dumps([['x-sandstorm-app-test','ok'],['x-sandstorm-user-id','forged'], ['x-sandstorm-permissions','deploy']])) )
    assert result['method'] == method, result
    assert result['headers']['x-sandstorm-user-id'] == '42'*32, result
    assert result['headers']['x-sandstorm-permissions'] == '', result
    assert result['headers']['cookie'] == 'test=cookie-value', result
    assert result['headers']['x-sandstorm-app-test'] == 'ok', result
    if method in ['POST','PUT','PATCH']:
        assert result['body'] == 'payload' and result['headers']['content-type'] == 'text/plain', result
        assert result['headers']['content-encoding'] == 'test-encoding', result
assert json.loads(request('form','name=Alice+Smith&count=2',SPKI_SMOKE_MIME='application/x-www-form-urlencoded')) == {'name':'Alice Smith','count':'2'}
assert json.loads(request('echo'))['headers']['x-sandstorm-user-id'] == ''
for code, expected in [(201,'content'),(204,'noContent'),(304,'notModified'),(302,'redirect'),(307,'redirect'),(308,'redirect'),(413,'clientError')]:
    result = json.loads(request(str(code),SPKI_SMOKE_INSPECT='1'))
    assert result['kind'] == expected, (code,result)
    assert result['cookies'][0]['httpOnly'] and result['cookies'][0]['value'] == 'abc', result
    assert ['x-sandstorm-app-test','retained'] in result['headers'], result
    if code == 201: assert result['status'] == 'Created' and result['etag'] == 'version'
    if code == 413: assert result['status'] == 'RequestEntityTooLarge'
    if expected == 'redirect':
        assert result['location'] == '/next' and result['permanent'] == (code == 308)
        assert result['switchToGet'] == (code == 302)
head = json.loads(request('200', SPKI_SMOKE_METHOD='HEAD', SPKI_SMOKE_INSPECT='1'))
assert head['kind'] == 'content' and head['body'] == '', head
# Restore the fixture before subsequent persistence tests.
request('__spki/v1/activate', json.dumps({'source':pathlib.Path('/opt/app/preview/fixtures/sdk-counter.js').read_text(),'expectedRevision':status()}),'api')
print('PASS: HTTP methods, form bodies, encoding, cookies, statuses, redirects, stable identity and spoof rejection')

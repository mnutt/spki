#!/usr/bin/env python3
"""Offline compiler tests; runs on the host without launching the app runtime."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
cli = root/'target/debug/spki'
env = dict(os.environ)
with tempfile.TemporaryDirectory() as temp:
    project = Path(temp)
    entry = project/'worker.ts'
    helper = project/'helper.ts'
    helper.write_text('export const message: string = "hello";')
    entry.write_text('import {message} from "./helper"; import {sandstorm} from "sandstorm:api"; export default {fetch(r: Request, env: unknown) { sandstorm(r,env); return new Response(message); }};')
    def build(name, ok=True):
        result = subprocess.run([str(cli),'build','--entry',str(entry),'--output',str(project/name)], env=env,capture_output=True,text=True,timeout=20)
        assert (result.returncode == 0) == ok, (result.stdout,result.stderr)
        return result
    build('out.js')
    code = (project/'out.js').read_text()
    assert 'sandstorm:api' in code and 'hello' in code and ': string' not in code
    assert str(project) not in code
    build('same.js')
    assert (project/'same.js').read_text() == code
    helper.write_text('export const message: string = "changed";')
    build('changed.js')
    assert 'changed' in (project/'changed.js').read_text()
    # The emitted bundle is never used when compilation fails.
    entry.write_text('import fs from "node:fs"; export default fs;')
    build('node.js',False)
    assert not (project/'node.js').exists()
    entry.write_text('export default {fetch() { return import(globalThis.moduleName); }};')
    build('dynamic.js',False)
    entry.write_text('export default {fetch() { return new Response("valid"); }};')
    build('out.js',False)
    assert (project/'out.js').read_text() == code
print('PASS: offline TS bundling, local dependency changes, SDK externals, deterministic output, Node/dynamic-import rejection, output protection')

import {BrowserCounter} from 'capnp:./counter.capnp';
import {connectBrowserNativeCapnp} from '/__sandstorm/native-capnp/client.js';
import './style.css';
const handoff = await fetch('/counter', {method:'POST'}).then(r => r.json());
const counter = connectBrowserNativeCapnp(handoff, BrowserCounter);
// Exposed only in this integration fixture, for session isolation and reload checks.
Object.assign(window, {counter, handoff});
document.querySelector('output')!.textContent = String((await counter.read()).value);
document.querySelector('button')!.addEventListener('click', async () => {
  document.querySelector('output')!.textContent = String((await counter.increment({amount: 1})).value);
});

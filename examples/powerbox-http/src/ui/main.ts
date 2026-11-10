import {App} from 'capnp:./schemas/app.capnp';
import {ApiSession} from '/__sandstorm/capnp/sandstorm/api-session.capnp.js';
import {
  connectApplication, apiSessionPowerboxDescriptor, requestPowerbox, claimBrowserNativeCapnpToken,
} from '/__sandstorm/native-capnp/client.js';
import './style.css';

const button = document.querySelector<HTMLButtonElement>('button')!;
const output = document.querySelector('pre')!;
let grant;
const app = await connectApplication(App);
button.disabled = false;
window.addEventListener('pagehide', () => { grant?.transport.close(); app.transport.close(); });
button.addEventListener('click', async () => {
  button.disabled = true;
  try {
    const descriptor = await apiSessionPowerboxDescriptor({canonicalUrl: 'https://example.com'});
    const requested = await requestPowerbox([descriptor], {saveLabel: {defaultText: 'Example HTTP connection'}});
    const next = await claimBrowserNativeCapnpToken(requested.token, ApiSession);
    grant?.transport.close();
    grant = next;
    output.textContent = (await app.read({api: grant.client})).text;
  } catch (error) {
    output.textContent = String(error);
  } finally {
    button.disabled = false;
  }
});

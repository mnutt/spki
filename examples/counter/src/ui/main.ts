import {Counter, CounterObserver} from 'capnp:./schemas/counter.capnp';
import {connectApplication} from '/__sandstorm/native-capnp/client.js';
import './style.css';

const heading = document.querySelector('h1')!;
const value = document.querySelector('#value')!;
const status = document.querySelector('#status')!;
const increment = document.querySelector<HTMLButtonElement>('#increment')!;
const reset = document.querySelector<HTMLButtonElement>('#reset')!;

function busy(disabled: boolean) {
  increment.disabled = disabled;
  reset.disabled = disabled;
}

async function connect() {
  const counter = await connectApplication(Counter);
  window.addEventListener('pagehide', () => counter.transport.close(), {once: true});
  const initial = await counter.read();
  heading.textContent = initial.greeting;
  // Pass a browser capability to the worker: it calls us when the count changes.
  const observer = new CounterObserver.Server({
    changed({value: next}) { value.textContent = String(next); },
  }).client();
  await counter.subscribe({observer});
  status.textContent = 'Connected through Cap’n Proto';
  busy(false);

  async function update(action: () => Promise<{value: number}>) {
    busy(true);
    try {
      await action();
      status.textContent = 'Connected through Cap’n Proto';
      busy(false);
    } catch (error) {
      status.textContent = `${String(error)} — reload to reconnect.`;
      counter.transport.close();
    }
  }
  increment.addEventListener('click', () => update(() => counter.increment()));
  reset.addEventListener('click', () => update(() => counter.reset()));
}

connect().catch(error => {
  status.textContent = `${String(error)} — reload to reconnect.`;
});

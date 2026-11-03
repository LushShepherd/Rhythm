// The native worker owns credentials, encryption, network requests and scheduling.
// This controller only reflects its state; it never stores a passphrase.
const STATES = new Set(['not_configured', 'disconnected', 'connecting', 'syncing', 'ready', 'error']);
const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function validSyncPassphrase(value) {
  if (typeof value !== 'string' || value.length > 1024 || [...value].length < 12) return false;
  const bytes = encoder.encode(value);
  return bytes.length <= 1024 && decoder.decode(bytes) === value;
}

export function readSyncStatus(value) {
  if (!value || typeof value !== 'object' || !STATES.has(value.state)
    || typeof value.connected !== 'boolean'
    || (value.last_success_at_utc_ms !== null && (!Number.isSafeInteger(value.last_success_at_utc_ms)
      || Math.abs(value.last_success_at_utc_ms) > 8_640_000_000_000_000))
    || (value.error !== null && typeof value.error !== 'string')) {
    throw new Error('autoSyncUnavailable');
  }
  return {
    state: value.state, connected: value.connected,
    reconnect_required: value.reconnect_required === true,
    last_success_at_utc_ms: value.last_success_at_utc_ms,
    error: value.error,
  };
}

export class AutomaticSyncController {
  constructor({ invoke, onChange = () => {} }) {
    if (typeof invoke !== 'function') throw new Error('Choose a native sync connection.');
    this.invoke = invoke;
    this.onChange = onChange;
    this.status = null;
    this.action = null;
    this.error = null;
    this.epoch = 0;
    this.reading = null;
  }

  // Native events invalidate older outstanding reads. A slow status response
  // must not restore a connection that the person has just disconnected.
  receive(value) {
    const status = readSyncStatus(value);
    this.epoch++;
    this.status = status;
    this.error = null;
    this.onChange();
  }

  fail(error) {
    this.error = error?.message ?? String(error);
    this.onChange();
  }

  refresh() {
    if (this.action) return Promise.resolve(false);
    if (this.reading) return this.reading;
    const epoch = this.epoch;
    this.reading = (async () => {
      try {
        const status = readSyncStatus(await Promise.resolve().then(() => this.invoke('sync_status')));
        if (epoch !== this.epoch) return false;
        this.status = status;
        this.error = null;
        this.onChange();
        return true;
      } catch (error) {
        if (epoch === this.epoch) this.fail(error);
        return false;
      } finally { this.reading = null; }
    })();
    return this.reading;
  }

  async perform(action, args = {}) {
    if (this.action) return false;
    this.epoch++;
    this.action = action;
    this.error = null;
    this.onChange();
    try {
      const status = readSyncStatus(await this.invoke(action, args));
      this.receive(status);
      return true;
    } catch (error) {
      this.fail(error);
      return false;
    } finally {
      this.action = null;
      this.onChange();
    }
  }

  connect(passphrase) {
    if (!validSyncPassphrase(passphrase)) {
      this.fail(new Error('autoSyncPhraseInvalid'));
      return Promise.resolve(false);
    }
    return this.perform('connect_sync', { passphrase });
  }

  retry() { return this.perform('retry_sync'); }
  disconnect() { return this.perform('disconnect_sync'); }
}

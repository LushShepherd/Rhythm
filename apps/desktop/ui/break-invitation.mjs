/** A default choice counts only time during which its invitation can be seen. */
export class BreakInvitationClock {
  #id = null;
  #elapsed = 0;
  #last = null;
  #counting = false;
  #fired = false;

  constructor({ durationMs = 12_000, now = () => performance.now() } = {}) {
    if (!Number.isFinite(durationMs) || durationMs <= 0) throw new RangeError('The invitation duration must be positive.');
    this.durationMs = durationMs;
    this.now = now;
  }

  reset(id) {
    if (typeof id !== 'string' || !id) throw new TypeError('An invitation needs a period ID.');
    if (id !== this.#id) {
      this.clear();
      this.#id = id;
    }
    return this.#snapshot();
  }

  clear() {
    this.#id = null;
    this.#elapsed = 0;
    this.#last = null;
    this.#counting = false;
    this.#fired = false;
    return this.#snapshot();
  }

  tick({ active, blocked = false }) {
    if (this.#id === null || this.#fired) return this.#snapshot();
    const current = this.now();
    const delta = this.#last === null ? 0 : current - this.#last;
    this.#last = Number.isFinite(current) ? current : null;

    // A suspended page or sleeping computer must not silently accept on return.
    const interrupted = !Number.isFinite(delta) || delta < 0 || delta > 1_000;
    if (!active || interrupted) this.#elapsed = 0;
    else if (!blocked && this.#counting) this.#elapsed += delta;

    this.#counting = Boolean(active && !blocked && Number.isFinite(current));
    if (this.#elapsed >= this.durationMs) {
      this.#elapsed = this.durationMs;
      this.#fired = true;
      return this.#snapshot(true);
    }
    return this.#snapshot();
  }

  #snapshot(ready = false) {
    return { id: this.#id, progress: this.#elapsed / this.durationMs, ready };
  }
}

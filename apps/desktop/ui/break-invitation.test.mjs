import test from 'node:test';
import assert from 'node:assert/strict';
import { BreakInvitationClock } from './break-invitation.mjs';

function fixture() {
  let time = 0;
  const clock = new BreakInvitationClock({ now: () => time });
  const tick = (milliseconds, options = { active: true }) => {
    time += milliseconds;
    return clock.tick(options);
  };
  const advance = (milliseconds, options) => {
    let result;
    while (milliseconds > 0) {
      const step = Math.min(100, milliseconds);
      result = tick(step, options);
      milliseconds -= step;
    }
    return result;
  };
  clock.reset('focus-1');
  tick(0);
  return { clock, tick, advance };
}

test('the default choice becomes ready after twelve visible seconds, once only', () => {
  const f = fixture();
  assert.equal(f.advance(11_999).ready, false);
  const deadline = f.tick(1);
  assert.deepEqual(deadline, { id: 'focus-1', progress: 1, ready: true });
  assert.deepEqual(f.tick(100), { id: 'focus-1', progress: 1, ready: false });
  f.clock.reset('focus-1');
  assert.equal(f.advance(12_000).ready, false);
});

test('leaving the app gives a fresh twelve-second opportunity when returning', () => {
  const f = fixture();
  assert.equal(f.advance(8_000).progress, 2 / 3);
  assert.equal(f.tick(100, { active: false }).progress, 0);
  assert.equal(f.tick(30_000, { active: false }).ready, false);
  assert.equal(f.tick(100).progress, 0);
  assert.equal(f.advance(11_900).ready, false);
  assert.equal(f.tick(100).ready, true);
});

test('a blocked choice pauses without charging interaction time on resume', () => {
  const f = fixture();
  assert.equal(f.advance(4_000).progress, 1 / 3);
  assert.equal(f.tick(100, { active: true, blocked: true }).progress, 1 / 3);
  assert.equal(f.advance(20_000, { active: true, blocked: true }).ready, false);
  assert.equal(f.tick(100).progress, 1 / 3);
  assert.equal(f.advance(7_900).ready, false);
  assert.equal(f.tick(100).ready, true);
});

test('sleep or suspended animation cannot immediately accept an old invitation', () => {
  const f = fixture();
  f.advance(11_900);
  assert.deepEqual(f.tick(60_000), { id: 'focus-1', progress: 0, ready: false });
  assert.equal(f.advance(11_900).ready, false);
  assert.equal(f.tick(100).ready, true);
});

test('re-rendering an invitation preserves its countdown; a new period replaces it', () => {
  const f = fixture();
  f.advance(6_000);
  assert.equal(f.clock.reset('focus-1').progress, 0.5);
  f.advance(5_000);
  assert.deepEqual(f.clock.reset('focus-2'), { id: 'focus-2', progress: 0, ready: false });
  assert.equal(f.tick(100).progress, 0);
  assert.equal(f.advance(11_900).ready, false);
  assert.deepEqual(f.tick(100), { id: 'focus-2', progress: 1, ready: true });
});

test('clearing a reminder cancels a nearly completed default choice', () => {
  const f = fixture();
  f.advance(11_900);
  assert.deepEqual(f.clock.clear(), { id: null, progress: 0, ready: false });
  assert.deepEqual(f.advance(20_000), { id: null, progress: 0, ready: false });
});

test('a clock discontinuity restarts the opportunity rather than accepting early', () => {
  const f = fixture();
  f.advance(11_900);
  assert.deepEqual(f.tick(-30_000), { id: 'focus-1', progress: 0, ready: false });
  assert.equal(f.advance(12_000).ready, true);
});

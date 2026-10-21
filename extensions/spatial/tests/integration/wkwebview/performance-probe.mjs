import { PERFORMANCE_PHASES } from './performance-data.mjs';

export function responsivenessProbe() {
  const started = performance.now();
  let last = started, current = 'open', frame, stopped = false;
  const phases = PERFORMANCE_PHASES.map(name => ({ name, sample_count: 0, over_100ms_count: 0, max_gap_ms: 0, elapsed_ms: 0 }));
  const over = [];
  const sample = now => {
    const gap = now - last;
    last = now;
    if (gap <= 0) return;
    const phase = phases.find(value => value.name === current);
    phase.sample_count++; phase.elapsed_ms += gap; phase.max_gap_ms = Math.max(phase.max_gap_ms, gap);
    if (gap > 100) { phase.over_100ms_count++; if (over.length < 64) over.push({ phase: current, gap_ms: gap }); }
  };
  const tick = () => { if (stopped) return; sample(performance.now()); frame = requestAnimationFrame(tick); };
  frame = requestAnimationFrame(tick);
  const snapshot = () => ({ elapsed_ms: performance.now() - started, sample_count: phases.reduce((sum, value) => sum + value.sample_count, 0), over_100ms_count: phases.reduce((sum, value) => sum + value.over_100ms_count, 0), max_gap_ms: Math.max(...phases.map(value => value.max_gap_ms)), phases: structuredClone(phases), over_100ms: structuredClone(over) });
  return { phase(name) { if (!PERFORMANCE_PHASES.includes(name)) throw new Error('INVALID_MEASUREMENT_PHASE'); current = name; }, snapshot,
    stop() { if (!stopped) { sample(performance.now()); stopped = true; cancelAnimationFrame(frame); } return snapshot(); } };
}

export function globalListenerProbe() {
  const prototype = EventTarget.prototype, add = prototype.addEventListener, remove = prototype.removeEventListener;
  const registrations = [];
  const capture = options => typeof options === 'boolean' ? options : Boolean(options?.capture);
  const erase = record => {
    const index = registrations.indexOf(record);
    if (index >= 0) registrations.splice(index, 1);
    if (record.signal && record.abort) remove.call(record.signal, 'abort', record.abort);
  };
  prototype.addEventListener = function (type, listener, options) {
    if ((this !== window && this !== document) || !listener || options?.signal?.aborted) return add.call(this, type, listener, options);
    const existing = registrations.find(value => value.target === this && value.type === type && value.listener === listener && value.capture === capture(options));
    if (existing) return add.call(this, type, existing.wrapped, options);
    const record = { target: this, type, listener, capture: capture(options), signal: options?.signal, abort: null, wrapped: null };
    record.wrapped = function (event) { if (options?.once) erase(record); if (typeof listener === 'function') listener.call(this, event); else listener.handleEvent(event); };
    record.abort = () => erase(record);
    add.call(this, type, record.wrapped, options);
    registrations.push(record);
    if (record.signal) add.call(record.signal, 'abort', record.abort, { once: true });
  };
  prototype.removeEventListener = function (type, listener, options) {
    const record = registrations.find(value => value.target === this && value.type === type && value.listener === listener && value.capture === capture(options));
    if (!record) return remove.call(this, type, listener, options);
    remove.call(this, type, record.wrapped, options); erase(record);
  };
  return { count: () => registrations.length, restore() { prototype.addEventListener = add; prototype.removeEventListener = remove; } };
}

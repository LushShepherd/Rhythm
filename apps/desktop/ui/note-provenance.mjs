// The UTC instant is authoritative. Source-zone information describes where the
// writing began and is never reconstructed for old entries from this device.
export function validateNoteTimezone(note) {
  const zone = note.source_timezone, offset = note.source_utc_offset_minutes;
  if (zone == null && offset == null) return {};
  if (typeof zone !== 'string' || !zone || zone.length > 128 || !/^[A-Za-z0-9/_+-]+$/.test(zone)
    || !Number.isInteger(offset) || offset < -1439 || offset > 1439) {
    throw new Error('The note contains invalid original time-zone information.');
  }
  try { new Intl.DateTimeFormat('en', { timeZone: zone }); }
  catch { throw new Error('The note contains an unsupported original time zone.'); }
  return { source_timezone: zone, source_utc_offset_minutes: offset };
}

export function captureNoteTimezone(utc, timeZone) {
  try {
    const formatter = new Intl.DateTimeFormat('en-GB-u-ca-iso8601-nu-latn', {
      timeZone: timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone,
      year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit',
      minute: '2-digit', second: '2-digit', hourCycle: 'h23',
    });
    const parts = Object.fromEntries(formatter.formatToParts(new Date(utc)).map(part => [part.type, part.value]));
    const wall = new Date(0);
    wall.setUTCFullYear(Number(parts.year), Number(parts.month) - 1, Number(parts.day));
    wall.setUTCHours(Number(parts.hour), Number(parts.minute), Number(parts.second), 0);
    const difference = wall.getTime() - Math.floor(utc / 1000) * 1000;
    if (difference % 60000 !== 0) return {};
    return validateNoteTimezone({ source_timezone: formatter.resolvedOptions().timeZone,
      source_utc_offset_minutes: difference / 60000 });
  } catch { return {}; }
}

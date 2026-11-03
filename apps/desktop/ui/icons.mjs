const paths = {
  extend: '<path d="M12 3a9 9 0 1 0 9 9M12 7v5l-3 2M19 2v6m-3-3h6"/>',
  spark: '<path d="m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5L12 3Z"/>',
  history: '<path d="M3 11a9 9 0 1 1 2.5 7M3 4v7h7M12 7v5l3 2"/>',
  rhythm: '<path d="M9 4a8 8 0 0 0 0 16M15 4a8 8 0 0 1 0 16"/><path d="M12 7v10" opacity=".5"/>',
  focus: '<path d="M19.5 4.5c-8-1-14 2-14 8a6 6 0 0 0 6 6c6 0 9-6 8-14Z"/><path d="m5 20 9-9M9 16v-4m0 4h4"/>',
  break: '<path d="M4 9h12v7a4 4 0 0 1-4 4H8a4 4 0 0 1-4-4V9Zm12 1h2a3 3 0 0 1 0 6h-2M7 3v2m5-2v2"/>',
  arrow: '<path d="M5 12h14m-5-5 5 5-5 5"/>',
  note: '<path d="M13 5H6a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-7"/><path d="m10 14 1-4 7-7a2.1 2.1 0 0 1 3 3l-7 7-4 1Zm7-10 3 3"/>',
  close: '<path d="m6 6 12 12M18 6 6 18"/>',
  expand: '<path d="M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5"/>',
  collapse: '<path d="M3 8h5V3m13 5h-5V3M8 21v-5H3m13 5v-5h5"/>',
  settings: '<path d="m10 3-.6 2.1-2 .9-2-.6-2 3.4L5 10.4v2.3l-1.6 1.6 2 3.4 2-.5 2 .9L10 21h4l.6-2.9 2-.9 2 .5 2-3.4-1.6-1.6v-2.3l1.6-1.6-2-3.4-2 .6-2-.9L14 3h-4Z"/><circle cx="12" cy="12" r="3"/>',
};
export function icon(name, className = '') {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ viewBox: '0 0 24 24', width: '24', height: '24', fill: 'none', stroke: 'currentColor', 'stroke-width': '1.5', 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true', focusable: 'false' })) svg.setAttribute(key, value);
  if (className) svg.setAttribute('class', className);
  svg.innerHTML = paths[name] ?? paths.rhythm;
  return svg;
}

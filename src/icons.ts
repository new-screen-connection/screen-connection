/** 图标注册表：统一 24x24 viewBox、1.7 描边风格。只用路径数据，不引入图标库。 */
export interface IconDef {
  paths: string[];
  circles?: Array<{ cx: number; cy: number; r: number }>;
  filled?: boolean;
}

export const ICONS: Record<string, IconDef> = {
  monitor: {
    paths: ['M2.5 4h19v12.5h-19z M9 20.5h6 M12 16.5v4'],
  },
  moon: { paths: ['M20 14.5A8 8 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5Z'] },
  sun: {
    circles: [{ cx: 12, cy: 12, r: 4 }],
    paths: ['M12 2v2 M12 20v2 M2 12h2 M20 12h2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M19.1 4.9l-1.4 1.4 M6.3 17.7l-1.4 1.4'],
  },
  palette: {
    paths: [
      'M12 3a9 9 0 1 0 0 18c1.4 0 2-.9 2-1.8 0-1.4-1.4-1.8-1.4-3 0-.9.7-1.6 1.7-1.6H16a5 5 0 0 0 5-5C21 6.2 17 3 12 3Z',
    ],
    circles: [
      { cx: 7.5, cy: 11, r: 1 },
      { cx: 10, cy: 7.5, r: 1 },
      { cx: 14.5, cy: 7.5, r: 1 },
    ],
  },
  bright: {
    circles: [{ cx: 12, cy: 12, r: 3.5 }],
    paths: ['M12 2.5v2 M12 19.5v2 M2.5 12h2 M19.5 12h2 M5.2 5.2l1.4 1.4 M17.4 17.4l1.4 1.4 M18.8 5.2l-1.4 1.4 M6.6 17.4l-1.4 1.4'],
  },
  hdr: { paths: ['M3 8v8 M7 8v8 M3 12h4 M14 8h2.5a4 4 0 0 1 0 8H14Z M20 8v8 M20 12h2'] },
  layout: { paths: ['M2.5 4.5h19v15h-19z M12 4.5v15 M5.5 8h3 M5.5 11h3'] },
  window: { paths: ['M2.5 5h19v14h-19z M2.5 9h19'] },
  rotate: { paths: ['M20 12a8 8 0 1 1-2.3-5.6 M20 3.5V8h-4.5'] },
  span: { paths: ['M2.5 5.5h8.5v13H2.5z M13 5.5h8.5v13H13z'] },
  restore: { paths: ['M4 9h11a5 5 0 0 1 0 10H8 M7.5 5.5 4 9l3.5 3.5'] },
  power: { paths: ['M12 3v8 M6.5 6.5a8 8 0 1 0 11 0'] },
  wrench: { paths: ['M15.5 3a5.5 5.5 0 0 0-4.9 8L3.5 18.1a1.8 1.8 0 0 0 2.5 2.5l7.1-7.1A5.5 5.5 0 1 0 15.5 3Z'] },
  clipboard: { paths: ['M5 4.5h14v16H5z M9 4.5v-1h6v1 M8.5 9.5h7 M8.5 13h7 M8.5 16.5h4'] },
  search: { paths: ['M15 15l5 5'], circles: [{ cx: 10.5, cy: 10.5, r: 6 }] },
  refresh: { paths: ['M20 12a8 8 0 1 1-2.3-5.6 M20 3.5V8h-4.5'] },
  backup: { paths: ['M4 7.5 12 3l8 4.5v9L12 21l-8-4.5Z M4 7.5 12 12l8-4.5 M12 12v9'] },
  lock: { paths: ['M4.5 10h15v10.5h-15z M8 10V7.5a4 4 0 0 1 8 0V10'] },
  shield: { paths: ['M12 2.8 20 6v6c0 4.6-3.3 8-8 9.2C7.3 20 4 16.6 4 12V6Z M8.5 12l2.3 2.3 4.7-4.7'] },
  gauge: { paths: ['M4 18a9 9 0 1 1 16 0 M12 13l4-4'], circles: [{ cx: 12, cy: 14, r: 1.4 }] },
  battery: { paths: ['M2.5 7h17v10h-17z M22 10.5v3 M5.5 10.5v3'] },
  headphone: { paths: ['M4 14v-2a8 8 0 0 1 16 0v2 M2.5 13.5h4v6h-4z M17.5 13.5h4v6h-4z'] },
  keyboard: { paths: ['M2.5 6h19v12h-19z M6 10h.5 M9.5 10h.5 M13 10h.5 M16.5 10h.5 M6 13.5h.5 M9.5 13.5h.5 M13 13.5h.5 M16.5 13.5h.5 M8 16h8'] },
  info: { paths: ['M12 11v5.5 M12 7.5v.5'], circles: [{ cx: 12, cy: 12, r: 9 }] },
  external: { paths: ['M13 4h7v7 M20 4 11 13 M18 14v4.5A1.5 1.5 0 0 1 16.5 20h-11A1.5 1.5 0 0 1 4 18.5v-11A1.5 1.5 0 0 1 5.5 6H10'] },
  update: { paths: ['M12 20V8 M7.5 12.5 12 8l4.5 4.5 M4 4h16'] },
  target: {
    paths: ['M12 12v0'],
    circles: [
      { cx: 12, cy: 12, r: 8.5 },
      { cx: 12, cy: 12, r: 4 },
      { cx: 12, cy: 12, r: 0.8 },
    ],
  },
  link: { paths: ['M10 13.5a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1.7 1.7 M14 10.5a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1.7-1.7'] },
  plug: { paths: ['M9 3v5 M15 3v5 M6 8h12v3a6 6 0 0 1-12 0Z M12 17v4'] },
  list: { paths: ['M8 6h13 M8 12h13 M8 18h13'], circles: [{ cx: 4, cy: 6, r: 1 }, { cx: 4, cy: 12, r: 1 }, { cx: 4, cy: 18, r: 1 }] },
  volume: { paths: ['M4 9.5v5h3.5L12 19V5L7.5 9.5H4Z M15.5 9a4.2 4.2 0 0 1 0 6 M18 6.8a8 8 0 0 1 0 10.4'] },
};

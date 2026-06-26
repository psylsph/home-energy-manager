import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const INDEX_CSS_PATH = resolve(process.cwd(), 'src/index.css');

describe('src/index.css — energy orbit node theme colours', () => {
  const source = readFileSync(INDEX_CSS_PATH, 'utf8');

  it('declares light theme node backgrounds so orbit symbols are not too dark', () => {
    const lightBlockMatch = source.match(/\[data-theme="light"\][^}]+\}/s);
    expect(lightBlockMatch).not.toBeNull();
    const lightBlock = lightBlockMatch![0];

    expect(lightBlock).toMatch(/--app-flow-node-solar-bg:\s*#FEF3C7;/);
    expect(lightBlock).toMatch(/--app-flow-node-grid-bg:\s*#FEE2E2;/);
    expect(lightBlock).toMatch(/--app-flow-node-home-bg:\s*#DBEAFE;/);
    expect(lightBlock).toMatch(/--app-flow-node-battery-bg:\s*#FEF3C7;/);
    expect(lightBlock).toMatch(/--app-flow-node-ev-bg:\s*#F3E8FF;/);
  });

  it('keeps dark theme node backgrounds distinct from light theme', () => {
    expect(source).toMatch(/--app-flow-node-home-bg:\s*#1D2D55;/);
    expect(source).toMatch(/--app-flow-node-ev-bg:\s*#2B1E3A;/);
  });
});

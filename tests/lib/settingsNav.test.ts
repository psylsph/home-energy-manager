import { describe, it, expect } from 'vitest';
import { activeSectionAt, groupOf, SETTINGS_NAV, SETTINGS_SECTION } from '../../src/lib/settingsNav';

describe('SETTINGS_NAV', () => {
  it('lists every section exactly once', () => {
    const ids = SETTINGS_NAV.flatMap((g) => g.sections.map((s) => s.id));
    expect(new Set(ids).size).toBe(ids.length);
    expect([...ids].sort()).toEqual(Object.values(SETTINGS_SECTION).sort());
  });

  it('groups sections by intent, in the order the issue asks for', () => {
    expect(SETTINGS_NAV.map((g) => g.label)).toEqual(['Connection', 'Energy & Cost', 'Alerts', 'Display', 'App']);
  });
});

describe('activeSectionAt', () => {
  const ids = ['a', 'b', 'c'];

  it('is the last section whose top has passed the line', () => {
    expect(activeSectionAt(ids, [-500, 100, 900], 140, false)).toBe('b');
    expect(activeSectionAt(ids, [-500, 140, 900], 140, false)).toBe('b');
    expect(activeSectionAt(ids, [-500, 141, 900], 140, false)).toBe('a');
  });

  it('is the first section before any has reached the line', () => {
    expect(activeSectionAt(ids, [300, 900, 1500], 140, false)).toBe('a');
  });

  it('is the last section at the bottom of the page, where short sections never reach the line', () => {
    expect(activeSectionAt(ids, [-900, 200, 600], 140, true)).toBe('c');
  });

  it('skips sections that are missing from the page', () => {
    expect(activeSectionAt(ids, [-500, Infinity, 50], 140, false)).toBe('c');
  });

  it('is empty with no sections', () => {
    expect(activeSectionAt([], [], 140, true)).toBe('');
  });
});

describe('groupOf', () => {
  it('finds the group holding a section, else the first group', () => {
    expect(groupOf(SETTINGS_NAV, SETTINGS_SECTION.weather)?.label).toBe('Energy & Cost');
    expect(groupOf(SETTINGS_NAV, SETTINGS_SECTION.about)?.label).toBe('App');
    expect(groupOf(SETTINGS_NAV, 'nope')?.label).toBe('Connection');
    expect(groupOf([], 'nope')).toBeUndefined();
  });
});

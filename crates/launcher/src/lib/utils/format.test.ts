import assert from 'node:assert/strict';
import { test } from 'node:test';
import { formatHotkeyStatus, formatInitial, formatLastPlayed, formatPlayTime } from './format.ts';

const MINUTE = 60_000;
const ago = (ms: number): string => new Date(Date.now() - ms).toISOString();

void test('formatPlayTime: no play time reads 0h', () => {
  assert.equal(formatPlayTime(0), '0h');
});

void test('formatPlayTime: under an hour shows minutes', () => {
  assert.equal(formatPlayTime(45), '45m');
});

void test('formatPlayTime: hours, with minutes only when verbose', () => {
  assert.equal(formatPlayTime(125), '2h');
  assert.equal(formatPlayTime(125, true), '2h 5m');
  assert.equal(formatPlayTime(120, true), '2h');
});

void test('formatLastPlayed: never played and unreadable dates', () => {
  assert.equal(formatLastPlayed(null), 'Never');
  assert.equal(formatLastPlayed('not a date'), 'Unknown');
});

void test('formatLastPlayed: counts back in the largest whole unit', () => {
  assert.equal(formatLastPlayed(ago(10_000)), 'Just now');
  assert.equal(formatLastPlayed(ago(1.5 * MINUTE)), '1 minute ago');
  assert.equal(formatLastPlayed(ago(3.5 * 60 * MINUTE)), '3 hours ago');
  assert.equal(formatLastPlayed(ago(2.5 * 24 * 60 * MINUTE)), '2 days ago');
  assert.equal(formatLastPlayed(ago(65 * 24 * 60 * MINUTE)), '2 months ago');
});

/** The instant of the UTC string `iso`, written at `minutes` east of UTC. */
const atOffset = (iso: string, minutes: number): string => {
  const wall = new Date(Date.parse(iso) + minutes * MINUTE).toISOString().slice(0, -1);
  const abs = Math.abs(minutes);
  const hours = String(Math.floor(abs / 60)).padStart(2, '0');
  const mins = String(abs % 60).padStart(2, '0');
  return `${wall}${minutes < 0 ? '-' : '+'}${hours}:${mins}`;
};

void test('formatLastPlayed: a UTC time and its local-offset form read the same', () => {
  assert.equal(
    formatLastPlayed('2026-11-05T19:19:09+01:00'),
    formatLastPlayed('2026-11-05T18:19:09Z'),
  );
  const utc = ago(3.5 * 60 * MINUTE);
  for (const minutes of [60, -480, 330]) {
    const written = atOffset(utc, minutes);
    assert.equal(Date.parse(written), Date.parse(utc), written);
    assert.equal(formatLastPlayed(written), '3 hours ago', written);
  }
});

void test('formatHotkeyStatus: ready when nothing failed', () => {
  assert.equal(formatHotkeyStatus([]), 'Hotkeys ready');
});

void test('formatHotkeyStatus: names every hotkey that failed', () => {
  assert.ok(formatHotkeyStatus(['F9', 'F10']).startsWith('F9, F10 unavailable'));
});

void test('formatInitial: a leading emoji and space are skipped', () => {
  assert.equal(formatInitial('\u{1F3AE} Foo'), 'F');
});

void test('formatInitial: punctuation and digits', () => {
  assert.equal(formatInitial('.zap//Q'), 'Z');
  assert.equal(formatInitial('3 Owls'), '3');
});

void test('formatInitial: a name without ASCII letters keeps its first letter', () => {
  assert.equal(formatInitial('\u{30AB}\u{30E1}'), '\u{30AB}');
  assert.equal(formatInitial('\u{0416}\u{0443}\u{043A}'), '\u{0416}');
});

void test('formatInitial: an astral first letter stays one code point', () => {
  const initial = formatInitial('\u{1D538}lpha');
  assert.equal(initial, '\u{1D538}');
  assert.equal(initial.codePointAt(0), 120120);
  assert.equal(initial.length, 2);
});

void test('formatInitial: a name of symbols only has no initial', () => {
  assert.equal(formatInitial('\u{1F3AE}'), '');
});

void test('formatInitial: an ASCII name is uppercased', () => {
  assert.equal(formatInitial('owls'), 'O');
});

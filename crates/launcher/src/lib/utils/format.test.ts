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

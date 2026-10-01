import assert from 'node:assert/strict';
import { test } from 'node:test';
import { formatHotkeyStatus, formatLastPlayed, formatPlayTime } from './format.ts';

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

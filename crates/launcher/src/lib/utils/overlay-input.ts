/**
 * `key` when it is one of `record`'s own keys, else null. A stored value from
 * a hand-edited file must not pass as a known one because it names something
 * every object inherits, such as `constructor` or `toString`.
 */
export function ownKeyOf<K extends string>(
  record: Record<K, unknown>,
  key: string | null | undefined,
): K | null {
  return key && Object.hasOwn(record, key) ? (key as K) : null;
}

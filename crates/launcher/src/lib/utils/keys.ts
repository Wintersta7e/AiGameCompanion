/**
 * Whether this keydown in the question input sends the question. The Enter
 * that confirms an IME composition only finishes it: sending then would send
 * the unconverted text. Some IMEs report that Enter only as keyCode 229.
 */
export function sendsQuestion(
  event: Pick<KeyboardEvent, 'key' | 'shiftKey' | 'isComposing' | 'keyCode'>,
): boolean {
  // eslint-disable-next-line @typescript-eslint/no-deprecated -- keyCode 229 is the only composition signal some IMEs give
  return event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229;
}

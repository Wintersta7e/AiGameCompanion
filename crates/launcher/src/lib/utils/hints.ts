// The follow-ups offered after a hinted answer. Each is sent as an ordinary
// question: the model reads from it how far to go.

/** The question "Another hint" sends. */
export const ANOTHER_HINT_TURN = 'Give me another hint.';
/** The question "Full answer" sends. */
export const FULL_ANSWER_TURN = 'Give me the full answer.';

/**
 * The parts of a chat message the follow-ups read.
 * @internal
 */
export interface ChipMsg {
  role: 'user' | 'assistant';
  content: string;
  streaming?: boolean;
  complete?: boolean;
  hinted?: boolean;
}

/**
 * Whether "Another hint" and "Full answer" show: the last message is a
 * finished answer to a question sent with hints first on, that question was
 * not already the full-answer one, hints first is still on, and a question
 * could be sent right now.
 */
export function showHintChips(
  messages: readonly ChipMsg[],
  hintsFirst: boolean,
  asking: boolean,
  canSend: boolean,
): boolean {
  const answer = messages.at(-1);
  const question = messages.at(-2);
  return (
    hintsFirst &&
    !asking &&
    canSend &&
    answer?.role === 'assistant' &&
    answer.complete === true &&
    answer.hinted === true &&
    answer.streaming !== true &&
    question?.role === 'user' &&
    question.content !== FULL_ANSWER_TURN
  );
}

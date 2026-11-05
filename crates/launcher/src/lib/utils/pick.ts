/**
 * The provider pick a window made that the launcher has not saved. A run that
 * could not read the library file saves nothing, and its pick must still hold
 * for the session: re-reading the stored provider, as every provider change
 * does, would otherwise put the old one back, and the next question would go
 * to a provider the user did not choose.
 */
export interface UnsavedPick<P> {
  /** The user picked `p`; it holds until the launcher saves it. */
  picked: (p: P) => void;
  /** The launcher saved the last pick, so the stored provider is it. */
  saved: () => void;
  /** The provider to show after the stored one, `stored`, is read again. */
  after: (stored: P | null) => P | null;
}

export function unsavedPick<P>(): UnsavedPick<P> {
  let unsaved: P | null = null;
  return {
    picked(p) {
      unsaved = p;
    },
    saved() {
      unsaved = null;
    },
    after(stored) {
      return unsaved ?? stored;
    },
  };
}

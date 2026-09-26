import { invoke } from '@tauri-apps/api/core';

/**
 * Open `url` in the default browser, once the backend accepts it as an https
 * web address.
 *
 * Call it only from the click (or middle-click) handler of the element the user
 * activated -- never on hover, focus, mount, render or a stream event. A refused
 * link changes nothing on screen: the rejection is the backend's fixed reason,
 * never the address.
 */
export function openLink(url: string): void {
  void invoke('open_url', { url }).catch((err: unknown) => {
    console.error('The link was not opened:', err);
  });
}

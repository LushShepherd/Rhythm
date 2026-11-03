/** Cache only the public application shell. Notes, images selected by the person,
 * learned preferences and sync credentials remain in their existing local stores.
 * Installation is optional and never prompts or reloads an open editor. */
export async function registerOffline(environment = globalThis) {
  if (environment.__TAURI__ || !environment.isSecureContext || !environment.navigator?.serviceWorker) {
    return { supported: false, ready: false, registration: null, error: null };
  }
  try {
    const registration = await environment.navigator.serviceWorker.register(
      new URL('./sw.js', import.meta.url), { scope: new URL('./', import.meta.url).href, updateViaCache: 'none' },
    );
    return { supported: true, ready: Boolean(registration.active), registration, error: null };
  } catch (error) {
    // A restricted/private browser can still run the ordinary local-storage app.
    return { supported: true, ready: false, registration: null,
      error: error instanceof Error ? error : new Error(String(error)) };
  }
}

let awaitingRestart = false;

/**
 * Records that the official profile was imported into this profile. Settings
 * held in memory still predate the import, so they must not be saved over it
 * until the app restarts.
 */
export function markProfileImportAwaitingRestart(): void {
  awaitingRestart = true;
}

/** True from a successful profile import until the app restarts. */
export function isProfileImportAwaitingRestart(): boolean {
  return awaitingRestart;
}

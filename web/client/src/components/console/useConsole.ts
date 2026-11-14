import { useEffect, useSyncExternalStore } from 'react';

import { SERVER_STATE, sharedConsoleStore, type ConsoleState, type ConsoleStore } from '../../chain/store';

/** The shared console store, read on first use. Reading the chain happens once per visit, not per render. */
export function useConsole(): { store: ConsoleStore; state: ConsoleState } {
  const store = sharedConsoleStore();
  const state = useSyncExternalStore(store.subscribe, store.getState, () => SERVER_STATE);
  useEffect(() => {
    void store.load();
  }, [store]);
  return { store, state };
}

import { useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-shell';
import { create } from 'zustand';
import { useShallow } from 'zustand/react/shallow';

import { ExportSettings } from '../../../ui/ExportImportProperties';

// Mirrors the serde shapes in src-tauri/src/publish/{types,session,commands}.rs.

export type AuthStatus =
  { status: 'NotConfigured' } | { status: 'NotAuthorised' } | { status: 'Connected'; account: string };

export interface AuthChallenge {
  authorize_url: string;
  instructions_key: string;
}

export interface DestinationInfo {
  id: string;
  display_name: string;
  capabilities: {
    supports_replace: boolean;
    supports_reconcile: boolean;
    supports_nested_containers: boolean;
    max_bytes: number | null;
    accepted_mime_types: string[];
  };
}

export interface PublishPreview {
  new: number;
  update: number;
  skip: number;
  unreadable: number;
}

export type ItemState = 'skipped' | 'uploaded' | 'updated' | 'failed' | 'ambiguous';

export interface PublishProgressEvent {
  completed: number;
  total: number;
  current_file: string;
  state: ItemState;
}

export interface SessionSummary {
  uploaded: number;
  updated: number;
  skipped: number;
  failed: Array<{ path: string; error: string }>;
  ambiguous: string[];
  cancelled: boolean;
}

export type SessionPhase = 'idle' | 'starting' | 'running' | 'cancelling' | 'complete' | 'cancelled' | 'error';

export interface PublishSessionState {
  phase: SessionPhase;
  completed: number;
  total: number;
  items: Array<{ file: string; state: ItemState }>;
  summary: SessionSummary | null;
  error: string | null;
}

export interface PublishTarget {
  albumId: string;
  exportSettings: ExportSettings;
  outputFormat: string;
}

const IDLE_SESSION: PublishSessionState = {
  phase: 'idle',
  completed: 0,
  total: 0,
  items: [],
  summary: null,
  error: null,
};

/**
 * `io:` errors come from rendering into the session's temporary storage and
 * can name paths inside it, which the panel never shows. The detail still
 * reaches the log.
 */
export const displayError = (error: unknown, localFileMessage: string): string => {
  const message = typeof error === 'string' ? error : error instanceof Error ? error.message : String(error);
  if (message.startsWith('io:')) {
    console.error('Publish:', message);
    return localFileMessage;
  }
  return message;
};

interface DestinationEntry {
  destination: DestinationInfo | null;
  authStatus: AuthStatus | null;
  authError: string | null;
  challenge: AuthChallenge | null;
}

const EMPTY_DESTINATION: DestinationEntry = {
  destination: null,
  authStatus: null,
  authError: null,
  challenge: null,
};

interface PublishStore {
  destinations: Record<string, DestinationEntry>;
  /** One for the app: the backend runs a single session, and its events name no destination. */
  session: PublishSessionState;
}

/**
 * Outside the component because the panel is a tab, and a tab unmounts while
 * another is active. The backend reports a session only through events and
 * cannot be asked what one is doing after the fact, so progress and a pending
 * authorisation have to outlive the panel.
 */
const usePublishStore = create<PublishStore>(() => ({ destinations: {}, session: IDLE_SESSION }));

const updateDestination = (destinationId: string, patch: Partial<DestinationEntry>) =>
  usePublishStore.setState((state) => ({
    destinations: {
      ...state.destinations,
      [destinationId]: { ...(state.destinations[destinationId] ?? EMPTY_DESTINATION), ...patch },
    },
  }));

const updateSession = (update: (current: PublishSessionState) => PublishSessionState) =>
  usePublishStore.setState((state) => ({ session: update(state.session) }));

let isListening = false;

/** Registered once and never removed, for the same reason the store is global. */
const listenForSessionEvents = () => {
  if (isListening) return;
  isListening = true;

  const finish = (phase: SessionPhase) => (event: { payload: SessionSummary }) =>
    updateSession((current) => ({ ...current, phase, summary: event.payload }));

  listen<PublishProgressEvent>('publish-progress', (event) => {
    const { completed, total, current_file, state } = event.payload;
    updateSession((current) => ({
      ...current,
      phase: current.phase === 'cancelling' ? 'cancelling' : 'running',
      completed,
      total,
      items: [...current.items, { file: current_file, state }],
    }));
  });
  listen<SessionSummary>('publish-complete', finish('complete'));
  listen<SessionSummary>('publish-cancelled', finish('cancelled'));
  listen<string>('publish-error', (event) => {
    updateSession((current) => ({ ...current, phase: 'error', error: event.payload }));
  });
};

/**
 * The publish commands and session events for one destination.
 */
export function usePublishState(destinationId: string, isActive: boolean) {
  const { destination, authStatus, authError, challenge } = usePublishStore(
    useShallow((state) => state.destinations[destinationId] ?? EMPTY_DESTINATION),
  );
  const session = usePublishStore((state) => state.session);

  useEffect(listenForSessionEvents, []);

  const refreshAuth = useCallback(async () => {
    updateDestination(destinationId, { authError: null });
    try {
      const [destinations, status] = await Promise.all([
        invoke<DestinationInfo[]>('publish_get_destinations'),
        invoke<AuthStatus>('publish_get_auth_status', { destinationId }),
      ]);
      updateDestination(destinationId, {
        destination: destinations.find((d) => d.id === destinationId) ?? null,
        authStatus: status,
      });
    } catch (error) {
      updateDestination(destinationId, { authError: String(error) });
    }
  }, [destinationId]);

  useEffect(() => {
    if (isActive && authStatus === null) refreshAuth();
  }, [isActive, authStatus, refreshAuth]);

  const setCredentials = useCallback(
    async (key: string, secret: string) => {
      await invoke('publish_set_credentials', { destinationId, key, secret });
      updateDestination(destinationId, { challenge: null });
      await refreshAuth();
    },
    [destinationId, refreshAuth],
  );

  const beginAuth = useCallback(async () => {
    const next = await invoke<AuthChallenge>('publish_begin_auth', { destinationId });
    updateDestination(destinationId, { challenge: next });
    await open(next.authorize_url);
  }, [destinationId]);

  const reopenAuthPage = useCallback(async () => {
    if (challenge) await open(challenge.authorize_url);
  }, [challenge]);

  const completeAuth = useCallback(
    async (verifier: string) => {
      try {
        await invoke('publish_complete_auth', { destinationId, verifier });
      } finally {
        // A verifier is spent whether or not the exchange succeeded, so a
        // retry needs a fresh challenge either way.
        updateDestination(destinationId, { challenge: null });
      }
      await refreshAuth();
    },
    [destinationId, refreshAuth],
  );

  const preview = useCallback(
    (target: PublishTarget) => invoke<PublishPreview>('publish_preview', { destinationId, ...target }),
    [destinationId],
  );

  const publish = useCallback(
    async (target: PublishTarget) => {
      updateSession(() => ({ ...IDLE_SESSION, phase: 'starting' }));
      try {
        await invoke('publish_album', { destinationId, ...target });
        updateSession((current) => (current.phase === 'starting' ? { ...current, phase: 'running' } : current));
      } catch (error) {
        updateSession(() => ({ ...IDLE_SESSION, phase: 'error', error: String(error) }));
      }
    },
    [destinationId],
  );

  const cancel = useCallback(async () => {
    updateSession((current) => ({ ...current, phase: 'cancelling' }));
    try {
      await invoke<boolean>('publish_cancel');
    } catch (error) {
      console.error('Failed to cancel publishing:', error);
      updateSession((current) => (current.phase === 'cancelling' ? { ...current, phase: 'running' } : current));
    }
  }, []);

  const dismissSession = useCallback(() => updateSession(() => IDLE_SESSION), []);

  return {
    destination,
    authStatus,
    authError,
    challenge,
    session,
    refreshAuth,
    setCredentials,
    beginAuth,
    reopenAuthPage,
    completeAuth,
    preview,
    publish,
    cancel,
    dismissSession,
  };
}

export type PublishStateApi = ReturnType<typeof usePublishState>;

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-shell';

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

/**
 * The publish commands and session events for one destination. Listeners are
 * held for the lifetime of the component, so a session keeps reporting while
 * the panel is hidden.
 */
export function usePublishState(destinationId: string, isActive: boolean) {
  const [destination, setDestination] = useState<DestinationInfo | null>(null);
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [authError, setAuthError] = useState<string | null>(null);
  const [challenge, setChallenge] = useState<AuthChallenge | null>(null);
  const [session, setSession] = useState<PublishSessionState>(IDLE_SESSION);

  const refreshAuth = useCallback(async () => {
    setAuthError(null);
    try {
      const [destinations, status] = await Promise.all([
        invoke<DestinationInfo[]>('publish_get_destinations'),
        invoke<AuthStatus>('publish_get_auth_status', { destinationId }),
      ]);
      setDestination(destinations.find((d) => d.id === destinationId) ?? null);
      setAuthStatus(status);
    } catch (error) {
      setAuthError(String(error));
    }
  }, [destinationId]);

  useEffect(() => {
    if (isActive && authStatus === null) refreshAuth();
  }, [isActive, authStatus, refreshAuth]);

  useEffect(() => {
    const finish = (phase: SessionPhase) => (event: { payload: SessionSummary }) =>
      setSession((current) => ({ ...current, phase, summary: event.payload }));

    const listeners = [
      listen<PublishProgressEvent>('publish-progress', (event) => {
        const { completed, total, current_file, state } = event.payload;
        setSession((current) => ({
          ...current,
          phase: current.phase === 'cancelling' ? 'cancelling' : 'running',
          completed,
          total,
          items: [...current.items, { file: current_file, state }],
        }));
      }),
      listen<SessionSummary>('publish-complete', finish('complete')),
      listen<SessionSummary>('publish-cancelled', finish('cancelled')),
      listen<string>('publish-error', (event) => {
        setSession((current) => ({ ...current, phase: 'error', error: event.payload }));
      }),
    ];

    return () => {
      listeners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);

  const setCredentials = useCallback(
    async (key: string, secret: string) => {
      await invoke('publish_set_credentials', { destinationId, key, secret });
      setChallenge(null);
      await refreshAuth();
    },
    [destinationId, refreshAuth],
  );

  const beginAuth = useCallback(async () => {
    const next = await invoke<AuthChallenge>('publish_begin_auth', { destinationId });
    setChallenge(next);
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
        setChallenge(null);
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
      setSession({ ...IDLE_SESSION, phase: 'starting' });
      try {
        await invoke('publish_album', { destinationId, ...target });
        setSession((current) => (current.phase === 'starting' ? { ...current, phase: 'running' } : current));
      } catch (error) {
        setSession({ ...IDLE_SESSION, phase: 'error', error: String(error) });
      }
    },
    [destinationId],
  );

  const cancel = useCallback(async () => {
    setSession((current) => ({ ...current, phase: 'cancelling' }));
    try {
      await invoke<boolean>('publish_cancel');
    } catch (error) {
      console.error('Failed to cancel publishing:', error);
      setSession((current) => (current.phase === 'cancelling' ? { ...current, phase: 'running' } : current));
    }
  }, []);

  const dismissSession = useCallback(() => setSession(IDLE_SESSION), []);

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

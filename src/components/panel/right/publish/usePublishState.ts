import { useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-shell';
import { create } from 'zustand';
import { useShallow } from 'zustand/react/shallow';

import { ExportSettings } from '../../../ui/ExportImportProperties';

// Mirrors the serde shapes in src-tauri/src/publish/{types,session,settings,links,commands}.rs.

export type AuthStatus =
  { status: 'NotConfigured' } | { status: 'NotAuthorised' } | { status: 'Connected'; account: string };

export interface AuthChallenge {
  authorize_url: string;
  instructions_key: string;
}

export type ContainerPrivacy = 'Public' | 'Unlisted' | 'Private';

export interface DestinationSettings {
  export_preset_id: string | null;
  /** Applied only when an album is created, never to one found or linked. */
  new_album_privacy: ContainerPrivacy;
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
    supported_privacy: ContainerPrivacy[];
  };
}

export type RemoteNodeKind = 'Folder' | 'Album';

/** One entry in the destination's album tree. `id` drills into a folder; `container` is what an album links by. */
export interface RemoteNode {
  id: string;
  container: string | null;
  kind: RemoteNodeKind;
  name: string;
  web_url: string | null;
  has_children: boolean;
}

export type LinkTarget = { kind: 'Existing'; remote_uri: string } | { kind: 'CreateNew'; name: string };

export interface LinkInfo {
  album_id: string;
  /** Null when the local album has been deleted. */
  album_name: string | null;
  /** Group names, outermost first. */
  album_path: string[];
  remote_uri: string;
  remote_name: string | null;
  web_url: string | null;
  last_published: string | null;
  broken: boolean;
}

/** What `publish_link_album` rejects with. */
export type LinkError =
  | { kind: 'AlreadyExists'; remote: RemoteNode }
  | { kind: 'AlreadyLinked'; album_id: string; album_name: string | null }
  | { kind: 'Failed'; message: string };

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

  const getSettings = useCallback(
    () => invoke<DestinationSettings>('publish_get_settings', { destinationId }),
    [destinationId],
  );

  const setSettings = useCallback(
    (settings: DestinationSettings) => invoke('publish_set_settings', { destinationId, settings }),
    [destinationId],
  );

  /** Keeps the API key and every link, so reconnecting resumes where it left off. */
  const disconnect = useCallback(async () => {
    await invoke('publish_disconnect', { destinationId });
    updateDestination(destinationId, { challenge: null });
    await refreshAuth();
  }, [destinationId, refreshAuth]);

  /** One level of the remote album tree; `parent` is a folder's `id`, or null for the root. */
  const listRemote = useCallback(
    (parent: string | null = null) => invoke<RemoteNode[]>('publish_list_remote', { destinationId, parent }),
    [destinationId],
  );

  const listLinks = useCallback(() => invoke<LinkInfo[]>('publish_list_links', { destinationId }), [destinationId]);

  /** Rejects with a `LinkError`. Linking to a different remote album drops the old one's photo records. */
  const linkAlbum = useCallback(
    (albumId: string, target: LinkTarget) => invoke<LinkInfo>('publish_link_album', { destinationId, albumId, target }),
    [destinationId],
  );

  /** Nothing on the destination is touched. */
  const unlink = useCallback(
    (albumId: string) => invoke('publish_unlink', { destinationId, albumId }),
    [destinationId],
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
    getSettings,
    setSettings,
    disconnect,
    listRemote,
    listLinks,
    linkAlbum,
    unlink,
    preview,
    publish,
    cancel,
    dismissSession,
  };
}

export type PublishStateApi = ReturnType<typeof usePublishState>;

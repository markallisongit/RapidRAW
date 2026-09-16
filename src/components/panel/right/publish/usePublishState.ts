import { useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-shell';
import { create } from 'zustand';
import { useShallow } from 'zustand/react/shallow';

// Mirrors the serde shapes in src-tauri/src/publish/{types,session,settings,links,preset,state,commands}.rs.

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

/** What the commands that need the destination's output preset reject with. */
export type PresetError = { kind: 'PresetMissing'; preset_id: string | null } | { kind: 'Failed'; message: string };

/** Why a photo was paired with a remote image. */
export type MatchReason = 'PublishName' | 'OriginalFileName' | 'CaptureTime' | 'LooksTheSame';

/** `Exact` is safe to adopt unseen; `Likely` starts ticked in the review, `Possible` unticked. */
export type MatchConfidence = 'Possible' | 'Likely' | 'Exact';

/** A photo paired with an image its linked remote album already holds. */
export interface ExistingPair {
  /** Virtual path. */
  path: string;
  remote_id: string;
  remote_file_name: string;
  remote_thumbnail_url: string | null;
  reasons: MatchReason[];
  confidence: MatchConfidence;
}

/** What an album's linked remote album already holds of its photos. */
export interface ExistingMatch {
  pairs: ExistingPair[];
  /** Every photo in the remote album, paired or not. */
  remote_photos: number;
  /** What publishing names the album's first unpublished photo; null when every photo is recorded. */
  example_file_name: string | null;
  /** A remote photo nothing was paired with. */
  example_remote_name: string | null;
}

/** How far matching has got comparing thumbnails. */
export interface MatchProgress {
  checked: number;
  total: number;
}

export interface PublishPreview {
  new: number;
  /** Edited since they were published. */
  update: number;
  /** Unedited, but published with different output settings. */
  settings_changed: number;
  skip: number;
  unreadable: number;
}

/** What publishing does with photos whose only change is the output settings. */
export type SettingsChangePolicy = 'Republish' | 'KeepExisting';

/** Published photos a switch of output preset would upload again. */
export interface SettingsImpact {
  photos: number;
  albums: number;
}

export interface RefreshReport {
  links_checked: number;
  renamed: number;
  broken: number;
  restored: number;
  images_missing: number;
  uploads_found: number;
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

/** One link's last preview. A preview being checked again keeps the counts it had. */
export interface LinkPreview {
  isChecking: boolean;
  preview: PublishPreview | null;
  /** A `PresetError` or a message; shown through `displayError`. */
  error: unknown;
}

const IDLE_SESSION: PublishSessionState = {
  phase: 'idle',
  completed: 0,
  total: 0,
  items: [],
  summary: null,
  error: null,
};

const isPresetError = (error: unknown): error is PresetError =>
  typeof error === 'object' && error !== null && 'kind' in error;

export const isPresetMissing = (error: unknown): boolean => isPresetError(error) && error.kind === 'PresetMissing';

const errorMessage = (error: unknown): string => {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  if (isPresetError(error)) {
    return error.kind === 'Failed' ? error.message : 'no output preset is chosen for this destination';
  }
  return String(error);
};

/**
 * `io:` errors come from rendering into the session's temporary storage and
 * can name paths inside it, which the panel never shows. The detail still
 * reaches the log.
 */
export const displayError = (error: unknown, localFileMessage: string): string => {
  const message = errorMessage(error);
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
  /** As last saved, so the panel follows what the manager saves. */
  settings: DestinationSettings | null;
  links: LinkInfo[] | null;
  linksError: unknown;
  isRefreshing: boolean;
  refreshReport: RefreshReport | null;
  refreshError: unknown;
  /** By local album id. */
  previews: Record<string, LinkPreview>;
  /** The link the panel summarises, kept while its tab is not showing. */
  selectedAlbumId: string | null;
}

const EMPTY_DESTINATION: DestinationEntry = {
  destination: null,
  authStatus: null,
  authError: null,
  challenge: null,
  settings: null,
  links: null,
  linksError: null,
  isRefreshing: false,
  refreshReport: null,
  refreshError: null,
  previews: {},
  selectedAlbumId: null,
};

export type ManagerSection = 'account' | 'apiKey' | 'output' | 'newAlbums';

/** Which destination the Publish Manager opens on, and the section to bring into view. */
export interface ManagerRequest {
  destinationId: string;
  section: ManagerSection | null;
}

interface PublishStore {
  destinations: Record<string, DestinationEntry>;
  /** Every destination the backend offers, in its order. */
  catalogue: DestinationInfo[] | null;
  /** One for the app: the backend runs a single session, and its events name no destination. */
  session: PublishSessionState;
  manager: ManagerRequest | null;
}

/**
 * Outside the component because the panel is a tab, and a tab unmounts while
 * another is active. The backend reports a session only through events and
 * cannot be asked what one is doing after the fact, so progress and a pending
 * authorisation have to outlive the panel.
 */
const usePublishStore = create<PublishStore>(() => ({
  destinations: {},
  catalogue: null,
  session: IDLE_SESSION,
  manager: null,
}));

const updateDestination = (destinationId: string, patch: Partial<DestinationEntry>) =>
  usePublishStore.setState((state) => ({
    destinations: {
      ...state.destinations,
      [destinationId]: { ...(state.destinations[destinationId] ?? EMPTY_DESTINATION), ...patch },
    },
  }));

const updatePreview = (destinationId: string, albumId: string, patch: Partial<LinkPreview>) =>
  usePublishStore.setState((state) => {
    const entry = state.destinations[destinationId] ?? EMPTY_DESTINATION;
    const current = entry.previews[albumId] ?? { isChecking: false, preview: null, error: null };
    return {
      destinations: {
        ...state.destinations,
        [destinationId]: { ...entry, previews: { ...entry.previews, [albumId]: { ...current, ...patch } } },
      },
    };
  });

/** Counts each destination's preview runs, so a newer run stops an older one. */
const previewRuns: Record<string, number> = {};

const updateSession = (update: (current: PublishSessionState) => PublishSessionState) =>
  usePublishStore.setState((state) => ({ session: update(state.session) }));

/** Needs no connection, so the panel need never have been shown. */
export const loadLinks = async (destinationId: string): Promise<LinkInfo[] | null> => {
  try {
    const next = await invoke<LinkInfo[]>('publish_list_links', { destinationId });
    updateDestination(destinationId, { links: next, linksError: null });
    return next;
  } catch (error) {
    updateDestination(destinationId, { linksError: error });
    return null;
  }
};

/** Every destination, with each one's links, for callers outside the panel. */
export const loadCatalogue = async (): Promise<DestinationInfo[]> => {
  const destinations = await invoke<DestinationInfo[]>('publish_get_destinations');
  usePublishStore.setState({ catalogue: destinations });
  await Promise.all(destinations.map((destination) => loadLinks(destination.id)));
  return destinations;
};

/** What is already known, without waiting on anything: the context menu is built synchronously. */
export const publishSnapshot = () => {
  const { catalogue, destinations } = usePublishStore.getState();
  return { catalogue, destinations };
};

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
  const {
    destination,
    authStatus,
    authError,
    challenge,
    settings,
    links,
    linksError,
    isRefreshing,
    refreshReport,
    refreshError,
    previews,
    selectedAlbumId,
  } = usePublishStore(useShallow((state) => state.destinations[destinationId] ?? EMPTY_DESTINATION));
  const session = usePublishStore((state) => state.session);

  useEffect(listenForSessionEvents, []);

  const refreshAuth = useCallback(async () => {
    updateDestination(destinationId, { authError: null });
    try {
      const [destinations, status] = await Promise.all([
        invoke<DestinationInfo[]>('publish_get_destinations'),
        invoke<AuthStatus>('publish_get_auth_status', { destinationId }),
      ]);
      usePublishStore.setState({ catalogue: destinations });
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

  const refreshSettings = useCallback(async () => {
    const next = await invoke<DestinationSettings>('publish_get_settings', { destinationId });
    updateDestination(destinationId, { settings: next });
    return next;
  }, [destinationId]);

  useEffect(() => {
    if (isActive && settings === null) refreshSettings().catch((error) => console.error('Publish settings:', error));
  }, [isActive, settings, refreshSettings]);

  /**
   * `keepExistingUploads` marks every published photo current with the new
   * preset before the store changes, so the panel never previews them as
   * changed in between.
   */
  const saveSettings = useCallback(
    async (next: DestinationSettings, keepExistingUploads = false) => {
      await invoke('publish_set_settings', { destinationId, settings: next });
      try {
        if (keepExistingUploads) await invoke<number>('publish_keep_existing_uploads', { destinationId });
      } finally {
        updateDestination(destinationId, { settings: next });
      }
    },
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

  const refreshLinks = useCallback(() => loadLinks(destinationId), [destinationId]);

  /** Reads remote state only; any changes are made to RapidRAW's local records. */
  const refreshRemote = useCallback(
    async (albumId: string | null = null) => {
      updateDestination(destinationId, { isRefreshing: true, refreshError: null });
      try {
        const report = await invoke<RefreshReport>('publish_refresh', { destinationId, albumId });
        updateDestination(destinationId, { isRefreshing: false, refreshReport: report });
        await refreshLinks();
        return report;
      } catch (error) {
        updateDestination(destinationId, { isRefreshing: false, refreshError: error });
        throw error;
      }
    },
    [destinationId, refreshLinks],
  );

  /** Rejects with a `LinkError`. Linking to a different remote album drops the old one's photo records. */
  const linkAlbum = useCallback(
    async (albumId: string, target: LinkTarget) => {
      const info = await invoke<LinkInfo>('publish_link_album', { destinationId, albumId, target });
      // Selected once listed, so the panel never sees a selection it has no link for.
      await refreshLinks();
      updateDestination(destinationId, { selectedAlbumId: albumId });
      return info;
    },
    [destinationId, refreshLinks],
  );

  /** Nothing on the destination is touched. */
  const unlink = useCallback(
    async (albumIds: string[]) => {
      try {
        for (const albumId of albumIds) await invoke('publish_unlink', { destinationId, albumId });
      } finally {
        await refreshLinks();
      }
    },
    [destinationId, refreshLinks],
  );

  const selectAlbum = useCallback(
    (albumId: string | null) => updateDestination(destinationId, { selectedAlbumId: albumId }),
    [destinationId],
  );

  /** Rejects with a `PresetError`. Reads the photos and the state file; never renders or uploads. */
  const preview = useCallback(
    async (albumId: string) => {
      updatePreview(destinationId, albumId, { isChecking: true });
      try {
        const counts = await invoke<PublishPreview>('publish_preview', { destinationId, albumId });
        updatePreview(destinationId, albumId, { isChecking: false, preview: counts, error: null });
        return counts;
      } catch (error) {
        updatePreview(destinationId, albumId, { isChecking: false, error });
        throw error;
      }
    },
    [destinationId],
  );

  /**
   * Rejects with a `PresetError`. Lists the linked remote album, reads the photos and compares thumbnails where
   * names and capture times leave pairs unsettled; records nothing. `cancelMatch` stops it, and it then rejects.
   */
  const matchExisting = useCallback(
    async (albumId: string, onProgress: (progress: MatchProgress) => void) => {
      const unlisten = await listen<MatchProgress>('publish-match-progress', (event) => onProgress(event.payload));
      try {
        return await invoke<ExistingMatch>('publish_match_existing', { destinationId, albumId });
      } finally {
        unlisten();
      }
    },
    [destinationId],
  );

  const cancelMatch = useCallback(() => invoke<boolean>('publish_cancel'), []);

  /**
   * Rejects with a `PresetError`. Records the chosen pairs that still hold as published, and returns how many; the
   * rest changed since they were matched. Then reloads the links and counts the album again. Nothing on the
   * destination changes.
   */
  const adoptExisting = useCallback(
    async (albumId: string, pairs: ExistingPair[]) => {
      const chosen = pairs.map(({ path, remote_id }) => ({ path, remote_id }));
      const recorded = await invoke<number>('publish_adopt_existing', { destinationId, albumId, pairs: chosen });
      await Promise.all([refreshLinks(), preview(albumId).catch(() => {})]);
      return recorded;
    },
    [destinationId, preview, refreshLinks],
  );

  /** A `data:` URL of the photo as edited. */
  const localThumbnail = useCallback((path: string) => invoke<string>('publish_local_thumbnail', { path }), []);

  /** A `data:` URL, or null when the destination has no thumbnail to give. */
  const remoteThumbnail = useCallback(
    (thumbnailUrl: string) => invoke<string | null>('publish_remote_thumbnail', { destinationId, thumbnailUrl }),
    [destinationId],
  );

  /**
   * One album at a time, since each preview reads every photo's sidecar. A
   * later call stops this one between albums.
   */
  const refreshPreviews = useCallback(
    async (albumIds: string[]) => {
      const run = (previewRuns[destinationId] ?? 0) + 1;
      previewRuns[destinationId] = run;
      for (const albumId of albumIds) {
        if (previewRuns[destinationId] !== run) return;
        await preview(albumId).catch(() => {});
      }
    },
    [destinationId, preview],
  );

  /** `onSettingsChange` is required so settings-only changes never upload without the user having chosen to. */
  const publish = useCallback(
    async (albumId: string, onSettingsChange: SettingsChangePolicy) => {
      updateSession(() => ({ ...IDLE_SESSION, phase: 'starting' }));
      try {
        await invoke('publish_album', { destinationId, albumId, onSettingsChange });
        updateSession((current) => (current.phase === 'starting' ? { ...current, phase: 'running' } : current));
      } catch (error) {
        updateSession(() => ({ ...IDLE_SESSION, phase: 'error', error: errorMessage(error) }));
      }
    },
    [destinationId],
  );

  /** Compares settings hashes only, so it is cheap however much is published. */
  const settingsImpact = useCallback(
    (exportPresetId: string) => invoke<SettingsImpact>('publish_settings_impact', { destinationId, exportPresetId }),
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
    settings,
    links,
    linksError,
    isRefreshing,
    refreshReport,
    refreshError,
    previews,
    selectedAlbumId,
    session,
    refreshAuth,
    setCredentials,
    beginAuth,
    reopenAuthPage,
    completeAuth,
    refreshSettings,
    saveSettings,
    disconnect,
    listRemote,
    refreshLinks,
    refreshRemote,
    linkAlbum,
    matchExisting,
    cancelMatch,
    adoptExisting,
    localThumbnail,
    remoteThumbnail,
    unlink,
    selectAlbum,
    preview,
    refreshPreviews,
    publish,
    settingsImpact,
    cancel,
    dismissSession,
  };
}

export type PublishStateApi = ReturnType<typeof usePublishState>;

/** The element focused when the manager opened, given focus back when it closes. */
let managerOpener: HTMLElement | null = null;

export function usePublishManager() {
  const request = usePublishStore((state) => state.manager);

  const openManager = useCallback((destinationId: string, section: ManagerSection | null = null) => {
    managerOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    usePublishStore.setState({ manager: { destinationId, section } });
  }, []);

  const closeManager = useCallback(() => {
    usePublishStore.setState({ manager: null });
    managerOpener?.focus();
    managerOpener = null;
  }, []);

  return { request, openManager, closeManager };
}

/** Every destination, loaded by the first `usePublishState` to check its connection. */
export const usePublishCatalogue = () => usePublishStore((state) => state.catalogue);

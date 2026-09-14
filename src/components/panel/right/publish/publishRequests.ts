import { create } from 'zustand';

import { Panel } from '../../../ui/AppProperties';
import { useUIStore } from '../../../../store/useUIStore';

/**
 * What another part of the app has asked the Publish panel to do. `publish`
 * starts publishing a linked album; `link` opens the link flow on an album.
 * The panel clears a request once it has acted on it, or stopped at a question.
 */
export interface PublishRequest {
  kind: 'publish' | 'link';
  destinationId: string;
  albumId: string;
}

interface PublishRequestStore {
  request: PublishRequest | null;
}

/** Kept apart from `useUIStore`, so publishing adds nothing to a file upstream owns. */
export const usePublishRequests = create<PublishRequestStore>(() => ({ request: null }));

const request = (next: PublishRequest) => {
  usePublishRequests.setState({ request: next });
  useUIStore.getState().setPanel(Panel.Publish);
};

export const requestPublish = (destinationId: string, albumId: string) =>
  request({ kind: 'publish', destinationId, albumId });

export const requestLink = (destinationId: string, albumId: string) =>
  request({ kind: 'link', destinationId, albumId });

/**
 * Clears `request` and returns true if it is still the pending one. False means it was taken
 * already: React runs a newly mounted panel's effects twice in development, and acting twice
 * would start two publishes.
 */
export const takePublishRequest = (request: PublishRequest): boolean => {
  if (usePublishRequests.getState().request !== request) return false;
  usePublishRequests.setState({ request: null });
  return true;
};

import i18n from 'i18next';
import { platform } from '@tauri-apps/plugin-os';
import { Link2, Send, UploadCloud } from 'lucide-react';

import { AlbumItem, OPTION_SEPARATOR, Option } from '../../../ui/AppProperties';
import { photosToPublish } from './PublishSummary';
import { requestLink, requestPublish } from './publishRequests';
import { loadCatalogue, publishSnapshot } from './usePublishState';

export { notifyDeletedLinks } from './deletedAlbumNotice';

// Publishing keeps credentials in the system keyring, which has no Android backend.
const isPublishSupported = (() => {
  try {
    return platform() !== 'android';
  } catch {
    return true;
  }
})();

// The menu is built synchronously, so what it shows has to be known before it opens. Links need no
// connection and change only through the publish store afterwards, so loading them once is enough.
if (isPublishSupported) loadCatalogue().catch((error) => console.error('Publish destinations:', error));

/**
 * `Publish to ▸` for an album in Sources: one entry per destination, publishing
 * a linked album or linking one that is not. Nothing for groups, or on Android.
 */
export const albumPublishOptions = (item: AlbumItem | null): Option[] => {
  if (!isPublishSupported || item?.type !== 'album') return [];
  const { catalogue, destinations } = publishSnapshot();
  if (!catalogue?.length) return [];

  const submenu: Option[] = catalogue.map((destination) => {
    const entry = destinations[destination.id];
    const name = destination.display_name;

    // Links not loaded yet: the panel works out which of the two applies.
    if (!entry?.links) {
      return {
        label: i18n.t('publish.albumMenu.publishNow', { destination: name }),
        icon: UploadCloud,
        onClick: () => requestPublish(destination.id, item.id),
      };
    }
    const link = entry.links.find((l) => l.album_id === item.id);
    if (!link) {
      return {
        label: i18n.t('publish.albumMenu.linkAndPublish', { destination: name }),
        icon: Link2,
        onClick: () => requestLink(destination.id, item.id),
      };
    }

    const preview = entry.previews[item.id]?.preview;
    const count = preview ? photosToPublish(preview) : 0;
    return {
      label:
        count > 0
          ? i18n.t('publish.albumMenu.publishNowCount', { destination: name, count })
          : i18n.t('publish.albumMenu.publishNow', { destination: name }),
      icon: UploadCloud,
      onClick: () => requestPublish(destination.id, item.id),
    };
  });

  return [{ type: OPTION_SEPARATOR }, { label: i18n.t('publish.albumMenu.publishTo'), icon: Send, submenu }];
};

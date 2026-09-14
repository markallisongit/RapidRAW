import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { ExternalLink } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'react-toastify';

import { AlbumItem } from '../../../ui/AppProperties';
import { DestinationInfo, LinkInfo, loadCatalogue, loadLinks, publishSnapshot } from './usePublishState';

/** A deleted album that was linked, named as it was before the delete: its link no longer knows. */
interface LeftBehind {
  albumName: string;
  link: LinkInfo;
}

const collectAlbums = (item: AlbumItem, into: Map<string, string> = new Map()): Map<string, string> => {
  if (item.type === 'album') into.set(item.id, item.name);
  else item.children.forEach((child) => collectAlbums(child, into));
  return into;
};

function OpenButton({ url, label }: { url: string; label: string }) {
  return (
    <button
      className="inline-flex items-center gap-1 text-accent hover:underline"
      onClick={() => open(url).catch((error) => console.error('Publish open:', error))}
    >
      {label}
      <ExternalLink size={12} />
    </button>
  );
}

function Notice({ destination, leftBehind }: { destination: DestinationInfo; leftBehind: LeftBehind[] }) {
  const { t } = useTranslation();
  const name = { destination: destination.display_name };
  /** For the sentences that use the destination's own word for an album. */
  const options = { ...name, context: destination.id };

  const renderBody = () => {
    if (leftBehind.length === 1) {
      const [{ albumName, link }] = leftBehind;
      return (
        <>
          <p className="text-text-secondary">
            {link.remote_name
              ? t('publish.deletedNotice.one', { ...options, album: albumName, remote: link.remote_name })
              : t('publish.deletedNotice.oneUnnamed', { ...options, album: albumName })}
          </p>
          {link.web_url && <OpenButton label={t('publish.deletedNotice.open', name)} url={link.web_url} />}
        </>
      );
    }
    return (
      <>
        <p className="text-text-secondary">
          {t('publish.deletedNotice.many', { ...options, count: leftBehind.length })}
        </p>
        <ul className="space-y-0.5">
          {leftBehind.map(({ albumName, link }) => (
            <li className="flex items-baseline gap-2" key={link.album_id}>
              <span className="min-w-0 truncate">
                {link.remote_name
                  ? t('publish.deletedNotice.line', { album: albumName, remote: link.remote_name })
                  : t('publish.deletedNotice.lineUnnamed', { ...options, album: albumName })}
              </span>
              {link.web_url && <OpenButton label={t('publish.deletedNotice.openShort')} url={link.web_url} />}
            </li>
          ))}
        </ul>
      </>
    );
  };

  return (
    <div className="space-y-1 text-sm">
      <p className="font-semibold">{t('publish.deletedNotice.title', name)}</p>
      {renderBody()}
    </div>
  );
}

/**
 * Called once `item` has been deleted from the album tree. Nothing is deleted on
 * any destination: the user is told what was left there, and the links, which
 * can never be published again, are removed. Never rejects, since the delete
 * has already happened.
 */
export const notifyDeletedLinks = async (item: AlbumItem): Promise<void> => {
  try {
    const albums = collectAlbums(item);
    if (albums.size === 0) return;
    const catalogue = publishSnapshot().catalogue ?? (await loadCatalogue());

    for (const destination of catalogue) {
      const links = await loadLinks(destination.id);
      const leftBehind = (links ?? [])
        .filter((link) => albums.has(link.album_id))
        .map((link) => ({ albumName: albums.get(link.album_id)!, link }));
      if (leftBehind.length === 0) continue;

      toast.info(<Notice destination={destination} leftBehind={leftBehind} />, {
        autoClose: false,
        closeOnClick: false,
      });

      for (const { link } of leftBehind) {
        await invoke('publish_unlink', { destinationId: destination.id, albumId: link.album_id }).catch((error) => {
          // Left for the panel's "links belong to deleted albums" line.
          console.error('Publish unlink after delete:', error);
        });
      }
      await loadLinks(destination.id);
    }
  } catch (error) {
    console.error('Publish deleted-album notice:', error);
  }
};

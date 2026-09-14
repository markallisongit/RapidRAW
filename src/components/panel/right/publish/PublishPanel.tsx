import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { Settings } from 'lucide-react';

import ConfirmModal from '../../../modals/ConfirmModal';
import Text from '../../../ui/Text';
import { AlbumItem, Panel } from '../../../ui/AppProperties';
import { ExportPreset } from '../../../ui/ExportImportProperties';
import { TextVariants } from '../../../../types/typography';
import { useLibraryStore } from '../../../../store/useLibraryStore';
import { useSettingsStore } from '../../../../store/useSettingsStore';
import { useUIStore } from '../../../../store/useUIStore';
import DestinationSection from './DestinationSection';
import LinkAlbumFlow from './LinkAlbumFlow';
import PublishManagerModal from './manager/PublishManagerModal';
import { formatSupport } from './output';
import PublishProgress from './PublishProgress';
import PublishSummary, { SettingsChangeQuestion } from './PublishSummary';
import { LinkInfo, SettingsChangePolicy, usePublishManager, usePublishState } from './usePublishState';

const DESTINATION_ID = 'smugmug';
const NO_PRESETS: ExportPreset[] = [];

const findAlbum = (items: AlbumItem[], albumId: string): { id: string; name: string } | null => {
  for (const item of items) {
    if (item.type === 'album' && item.id === albumId) return { id: item.id, name: item.name };
    if (item.type === 'group') {
      const found = findAlbum(item.children, albumId);
      if (found) return found;
    }
  }
  return null;
};

/** The link flow in progress: the album it starts on, if any, and whether that album is linked already. */
interface Flow {
  albumId: string | null;
  isRelink: boolean;
}

type Confirm =
  | { kind: 'unlink'; link: LinkInfo }
  | { kind: 'relink'; link: LinkInfo }
  | { kind: 'removeDeleted'; links: LinkInfo[] };

/** Session, authorisation, links and previews live in `usePublishState`'s store, so switching tabs loses none of them. */
export default function PublishPanel() {
  const { t } = useTranslation();
  const isVisible = useUIStore((state) => Object.values(state.activePanels).includes(Panel.Publish));
  const api = usePublishState(DESTINATION_ID, isVisible);
  const { authStatus, destination, session, settings, links, previews, selectedAlbumId } = api;
  const { refreshLinks, refreshPreviews, selectAlbum } = api;
  const destinationName = destination?.display_name ?? 'SmugMug';
  const { openManager } = usePublishManager();

  const presets = useSettingsStore((state) => state.appSettings?.exportPresets) ?? NO_PRESETS;
  const { albumTree, activeAlbumId } = useLibraryStore(
    useShallow((state) => ({ albumTree: state.albumTree, activeAlbumId: state.activeAlbumId })),
  );

  const isConnected = authStatus?.status === 'Connected';
  const isIdle = session.phase === 'idle';
  const presetId = settings?.export_preset_id ?? null;
  const preset = presetId === null ? null : (presets.find((p) => p.id === presetId) ?? null);
  const canCheck = isConnected && preset !== null && formatSupport(destination, preset.fileFormat).isAccepted;

  const [flow, setFlow] = useState<Flow | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [question, setQuestion] = useState<{ albumId: string; count: number } | null>(null);
  const [isStarting, setIsStarting] = useState(false);

  // Names, deletions and broken links can change with the album tree and after a publish.
  useEffect(() => {
    if (isVisible && isConnected && isIdle) refreshLinks();
  }, [isVisible, isConnected, isIdle, albumTree, refreshLinks]);

  // Checked again whenever the links, the preset or its settings change, and each time the tab
  // is shown, which is how edits made in other panels come to be counted.
  useEffect(() => {
    if (!isVisible || !canCheck || !isIdle || !links) return;
    refreshPreviews(links.filter((link) => link.album_name !== null && !link.broken).map((link) => link.album_id));
    // An empty run stops this one between albums.
    return () => void refreshPreviews([]);
  }, [isVisible, canCheck, isIdle, links, preset, refreshPreviews]);

  const linksRef = useRef(links);
  linksRef.current = links;
  // Links whose RapidRAW album was deleted are never selected: there is nothing left to publish.
  const isLinked = useCallback(
    (albumId: string | null) =>
      !!albumId && !!linksRef.current?.some((l) => l.album_id === albumId && l.album_name !== null),
    [],
  );

  // The album selected in Sources is the one summarised, when it is linked.
  useEffect(() => {
    if (isLinked(activeAlbumId)) selectAlbum(activeAlbumId);
  }, [activeAlbumId, isLinked, selectAlbum]);

  useEffect(() => {
    if (!links || isLinked(selectedAlbumId)) return;
    const firstLive = links.find((link) => link.album_name !== null);
    selectAlbum(isLinked(activeAlbumId) ? activeAlbumId : (firstLive?.album_id ?? null));
  }, [links, selectedAlbumId, activeAlbumId, isLinked, selectAlbum]);

  const activeAlbum = useMemo(
    () => (activeAlbumId ? findAlbum(albumTree, activeAlbumId) : null),
    [albumTree, activeAlbumId],
  );
  const unlinkedActiveAlbum = activeAlbum && links && !isLinked(activeAlbum.id) ? activeAlbum : null;
  const selectedLink = links?.find((link) => link.album_id === selectedAlbumId) ?? null;

  /** Checks again first, so the settings question counts what is true now. */
  const startPublish = async (albumId: string) => {
    selectAlbum(albumId);
    setIsStarting(true);
    try {
      const counts = await api.preview(albumId);
      if (counts.settings_changed > 0) {
        setQuestion({ albumId, count: counts.settings_changed });
      } else {
        // With nothing whose only change is its settings, there is nothing to ask about.
        await api.publish(albumId, 'KeepExisting');
      }
    } catch {
      // The preview's error is shown in the summary.
    } finally {
      setIsStarting(false);
    }
  };

  const answer = (policy: SettingsChangePolicy) => {
    if (!question) return;
    setQuestion(null);
    api.publish(question.albumId, policy);
  };

  const renderBody = () => {
    if (!isIdle) return <PublishProgress api={api} destinationName={destinationName} />;

    if (flow && links) {
      return (
        <LinkAlbumFlow
          albumTree={albumTree}
          api={api}
          destinationName={destinationName}
          initialAlbumId={flow.albumId}
          isRelink={flow.isRelink}
          links={links}
          onCancel={() => setFlow(null)}
          onChangePrivacy={() => openManager(DESTINATION_ID, 'newAlbums')}
          onDone={() => setFlow(null)}
          privacy={settings?.new_album_privacy ?? 'Public'}
        />
      );
    }

    return (
      <>
        <div className="grow overflow-y-auto p-3">
          <DestinationSection
            api={api}
            canCheck={canCheck}
            destinationName={destinationName}
            onLinkAlbum={(albumId) => setFlow({ albumId, isRelink: false })}
            onOpenManager={(section) => openManager(DESTINATION_ID, section)}
            onPublish={startPublish}
            onRemoveDeleted={(deleted) => setConfirm({ kind: 'removeDeleted', links: deleted })}
            onRelink={(link) => setConfirm({ kind: 'relink', link })}
            onUnlink={(link) => setConfirm({ kind: 'unlink', link })}
            preset={preset}
            unlinkedActiveAlbum={unlinkedActiveAlbum}
          />
        </div>
        {canCheck && selectedLink && preset && (
          <PublishSummary
            destinationId={DESTINATION_ID}
            destinationName={destinationName}
            entry={previews[selectedLink.album_id]}
            isStarting={isStarting}
            link={selectedLink}
            onPublish={() => startPublish(selectedLink.album_id)}
            preset={preset}
          />
        )}
      </>
    );
  };

  const confirmText = (): { title: string; message: string; confirm: string } => {
    if (confirm?.kind === 'removeDeleted') {
      return {
        title: t('publish.links.confirmRemoveDeleted.title', { count: confirm.links.length }),
        message: t('publish.links.confirmRemoveDeleted.message', {
          count: confirm.links.length,
          destination: destinationName,
        }),
        confirm: t('publish.links.removeDeleted'),
      };
    }
    const name = confirm?.link.album_name ?? '';
    if (confirm?.kind === 'relink') {
      return {
        title: t('publish.links.confirmRelink.title', { name, context: DESTINATION_ID }),
        message: confirm.link.remote_name
          ? t('publish.links.confirmRelink.message', {
              remote: confirm.link.remote_name,
              destination: destinationName,
              context: DESTINATION_ID,
            })
          : t('publish.links.confirmRelink.messageUnnamed', { destination: destinationName, context: DESTINATION_ID }),
        confirm: t('publish.links.confirmRelink.confirm', { context: DESTINATION_ID }),
      };
    }
    return {
      title: t('publish.links.confirmUnlink.title', { name }),
      message: t('publish.links.confirmUnlink.message', { destination: destinationName }),
      confirm: t('publish.links.menu.unlink'),
    };
  };
  const confirmation = confirmText();

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 flex justify-between items-center shrink-0 border-b border-surface">
        <Text variant={TextVariants.title}>{t('publish.panel.title')}</Text>
        <button
          aria-label={t('publish.manager.title')}
          className="p-1.5 rounded-md text-text-secondary hover:text-text-primary hover:bg-surface transition-colors"
          data-tooltip={t('publish.manager.title')}
          onClick={() => openManager(DESTINATION_ID)}
        >
          <Settings size={18} />
        </button>
      </div>

      {renderBody()}

      <PublishManagerModal />

      <ConfirmModal
        confirmText={confirmation.confirm}
        isOpen={confirm !== null}
        message={confirmation.message}
        onClose={() => setConfirm(null)}
        onConfirm={() => {
          if (!confirm) return;
          if (confirm.kind === 'relink') {
            setFlow({ albumId: confirm.link.album_id, isRelink: true });
            return;
          }
          const albumIds = confirm.kind === 'unlink' ? [confirm.link.album_id] : confirm.links.map((l) => l.album_id);
          api.unlink(albumIds).catch((error) => console.error('Publish unlink:', error));
        }}
        title={confirmation.title}
      />

      <SettingsChangeQuestion
        count={question?.count ?? null}
        onCancel={() => setQuestion(null)}
        onKeepExisting={() => answer('KeepExisting')}
        onRepublish={() => answer('Republish')}
      />
    </div>
  );
}

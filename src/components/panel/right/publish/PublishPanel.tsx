import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { motion } from 'framer-motion';
import { AlertTriangle, ArrowUpRight, Loader, Settings, UploadCloud } from 'lucide-react';

import Button from '../../../ui/Button';
import Dropdown from '../../../ui/Dropdown';
import Text from '../../../ui/Text';
import { AlbumItem, Panel } from '../../../ui/AppProperties';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import { useExportSettings } from '../../../../hooks/useExportSettings';
import { useLibraryStore } from '../../../../store/useLibraryStore';
import { useSettingsStore } from '../../../../store/useSettingsStore';
import { useUIStore } from '../../../../store/useUIStore';
import PublishManagerModal from './manager/PublishManagerModal';
import { LAST_USED_PRESET_ID, describeOutput, formatOf, formatSupport, toExportSettings } from './output';
import PublishProgress from './PublishProgress';
import {
  PublishPreview,
  PublishTarget,
  displayError,
  isPresetMissing,
  usePublishManager,
  usePublishState,
} from './usePublishState';

const DESTINATION_ID = 'smugmug';

interface PublishableAlbum {
  id: string;
  label: string;
  /** The name the backend gives the remote album: groups folded in. */
  remoteName: string;
  images: string[];
}

const flattenAlbums = (items: AlbumItem[], parents: string[] = []): PublishableAlbum[] =>
  items.flatMap((item) =>
    item.type === 'album'
      ? [
          {
            id: item.id,
            label: [...parents, item.name].join(' › '),
            remoteName: [...parents, item.name].join(' - '),
            images: item.images,
          },
        ]
      : flattenAlbums(item.children, [...parents, item.name]),
  );

function Warning({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-2 bg-yellow-500/10 rounded-md p-3">
      <AlertTriangle size={16} className="shrink-0 mt-0.5 text-yellow-400" />
      <Text variant={TextVariants.small} color={TextColors.primary}>
        {children}
      </Text>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <Text variant={TextVariants.heading} className="mb-2">
        {title}
      </Text>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

/** Session and authorisation state live in `usePublishState`'s store, so switching tabs loses neither. */
export default function PublishPanel() {
  const { t } = useTranslation();
  const isVisible = useUIStore((state) => Object.values(state.activePanels).includes(Panel.Publish));
  const api = usePublishState(DESTINATION_ID, isVisible);
  const { authStatus, authError, destination, session, settings } = api;
  const destinationName = destination?.display_name ?? 'SmugMug';
  const { openManager } = usePublishManager();

  const appSettings = useSettingsStore((state) => state.appSettings);
  const setPanel = useUIStore((state) => state.setPanel);
  const { albumTree, activeAlbumId } = useLibraryStore(
    useShallow((state) => ({ albumTree: state.albumTree, activeAlbumId: state.activeAlbumId })),
  );

  // Interim, until #21 requires a preset: with none chosen, publishing uses
  // the Export panel's last-used settings, through the same hook and defaults.
  const { currentSettingsObject, handleApplyPreset } = useExportSettings();
  const lastUsedPreset = appSettings?.exportPresets?.find((p) => p.id === LAST_USED_PRESET_ID);
  useEffect(() => {
    if (lastUsedPreset) handleApplyPreset(lastUsedPreset);
  }, [lastUsedPreset, handleApplyPreset]);

  const albums = useMemo(() => flattenAlbums(albumTree), [albumTree]);
  const [selectedAlbumId, setSelectedAlbumId] = useState<string | null>(null);
  useEffect(() => {
    if (activeAlbumId && albums.some((a) => a.id === activeAlbumId)) setSelectedAlbumId(activeAlbumId);
  }, [activeAlbumId, albums]);
  const album = albums.find((a) => a.id === selectedAlbumId) ?? null;

  const presetId = settings?.export_preset_id ?? null;
  const preset = presetId === null ? null : (appSettings?.exportPresets?.find((p) => p.id === presetId) ?? null);
  // What publishing renders with; unknown when the chosen preset has been deleted.
  const output = presetId === null ? currentSettingsObject : preset;
  const outputFormat = formatOf(currentSettingsObject.fileFormat).extensions[0];
  const support = formatSupport(destination, output?.fileFormat ?? currentSettingsObject.fileFormat);
  const isFormatAccepted = support.isAccepted;

  const target: PublishTarget | null = useMemo(
    () => (album ? { albumId: album.id, exportSettings: toExportSettings(currentSettingsObject), outputFormat } : null),
    [album, currentSettingsObject, outputFormat],
  );

  const isConnected = authStatus?.status === 'Connected';
  const [preview, setPreview] = useState<PublishPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [isPreviewing, setIsPreviewing] = useState(false);

  useEffect(() => {
    setPreview(null);
    setPreviewError(null);
    if (!isVisible || !isConnected || !target || !output || !isFormatAccepted || session.phase !== 'idle') return;

    let isCurrent = true;
    setIsPreviewing(true);
    api
      .preview(target)
      .then((counts) => isCurrent && setPreview(counts))
      .catch(
        (error) =>
          isCurrent &&
          setPreviewError(
            isPresetMissing(error)
              ? t('publish.errors.presetMissing')
              : displayError(error, t('publish.errors.localFile')),
          ),
      )
      .finally(() => isCurrent && setIsPreviewing(false));
    return () => {
      isCurrent = false;
    };
  }, [isVisible, isConnected, target, album?.images, output, isFormatAccepted, session.phase, api.preview, t]);

  // Interim, until #21 asks: settings-only changes republish, as they always have.
  const toPublish = preview ? preview.new + preview.update + preview.settings_changed : 0;
  const canPublish =
    isConnected && !!album && album.images.length > 0 && output !== null && isFormatAccepted && !isPreviewing;

  const renderBody = () => {
    if (authError) {
      return (
        <div className="space-y-3">
          <Text color={TextColors.error}>{displayError(authError, t('publish.errors.localFile'))}</Text>
          <Button className="bg-surface text-text-primary shadow-none" onClick={api.refreshAuth}>
            {t('publish.panel.retry')}
          </Button>
        </div>
      );
    }
    if (!authStatus) {
      return (
        <Text className="flex items-center gap-2 italic">
          <Loader size={16} className="animate-spin" /> {t('publish.panel.loading')}
        </Text>
      );
    }
    if (authStatus.status !== 'Connected') {
      const hasKey = authStatus.status === 'NotAuthorised';
      return (
        <div className="space-y-3">
          <Text>
            {hasKey
              ? t('publish.prompt.notConnected', { destination: destinationName })
              : t('publish.prompt.notSetUp', { destination: destinationName })}
          </Text>
          <Button onClick={() => openManager(DESTINATION_ID, hasKey ? 'account' : 'apiKey')}>
            <Settings size={16} />
            {hasKey
              ? t('publish.prompt.connect', { destination: destinationName })
              : t('publish.prompt.setUp', { destination: destinationName })}
          </Button>
        </div>
      );
    }

    return (
      <>
        <Section title={t('publish.connected.heading')}>
          <Text color={TextColors.primary}>{t('publish.connected.account', { account: authStatus.account })}</Text>
        </Section>

        <Section title={t('publish.album.heading')}>
          {albums.length === 0 ? (
            <Text>{t('publish.album.noAlbums')}</Text>
          ) : (
            <>
              <Dropdown
                className="w-full"
                options={albums.map((a) => ({ label: a.label, value: a.id }))}
                placeholder={t('publish.album.placeholder')}
                value={selectedAlbumId}
                onChange={setSelectedAlbumId}
              />
              {album && (
                <Text variant={TextVariants.small}>
                  {album.images.length === 0
                    ? t('publish.album.empty')
                    : t('publish.album.mapping', { name: album.remoteName })}
                </Text>
              )}
            </>
          )}
        </Section>

        <Section title={t('publish.settings.heading')}>
          {output ? (
            <>
              <Text>
                {preset ? t('publish.settings.preset', { name: preset.name }) : t('publish.settings.current')}
              </Text>
              <Text color={TextColors.primary} weight={TextWeights.medium}>
                {describeOutput(output, t)}
              </Text>
            </>
          ) : (
            <Warning>{t('publish.errors.presetMissing')}</Warning>
          )}
          {!isFormatAccepted && (
            <Warning>
              {t(preset ? 'publish.settings.unsupportedPresetFormat' : 'publish.settings.unsupportedFormat', {
                destination: destinationName,
                formats: support.acceptedNames,
              })}
            </Warning>
          )}
          {presetId === null && (
            <button
              className="flex items-center gap-1 text-sm text-accent hover:underline"
              onClick={() => setPanel(Panel.Export)}
            >
              {t('publish.settings.openExport')}
              <ArrowUpRight size={14} />
            </button>
          )}
          <button
            className="flex items-center gap-1 text-sm text-accent hover:underline"
            onClick={() => openManager(DESTINATION_ID, 'output')}
          >
            {preset ? t('publish.settings.changePreset') : t('publish.settings.choosePreset')}
            <ArrowUpRight size={14} />
          </button>
        </Section>

        {album && album.images.length > 0 && output && isFormatAccepted && (
          <Section title={t('publish.preview.heading')}>
            {isPreviewing ? (
              <Text className="flex items-center gap-2 italic">
                <Loader size={14} className="animate-spin" /> {t('publish.preview.checking')}
              </Text>
            ) : previewError ? (
              <Text color={TextColors.error}>{t('publish.preview.failed', { error: previewError })}</Text>
            ) : (
              preview && (
                <>
                  <Text color={TextColors.primary}>
                    {t('publish.preview.counts', {
                      unchanged: preview.skip,
                      update: preview.update + preview.settings_changed,
                      new: preview.new,
                    })}
                  </Text>
                  {preview.unreadable > 0 && (
                    <Text variant={TextVariants.small} color={TextColors.error}>
                      {t('publish.preview.unreadable', { count: preview.unreadable })}
                    </Text>
                  )}
                </>
              )
            )}
          </Section>
        )}
      </>
    );
  };

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

      {session.phase !== 'idle' ? (
        <PublishProgress api={api} destinationName={destinationName} />
      ) : (
        <>
          <div className="grow overflow-y-auto p-3 space-y-8">{renderBody()}</div>
          {isConnected && (
            <div className="p-3 border-t border-surface shrink-0">
              <motion.div
                whileTap={canPublish ? { scale: 0.98 } : undefined}
                transition={{ type: 'spring', stiffness: 400, damping: 17 }}
                className="w-full"
              >
                <Button
                  className="rounded-md h-11 w-full flex items-center text-md font-bold! justify-center"
                  disabled={!canPublish}
                  onClick={() => target && api.publish(target, 'Republish')}
                  size="lg"
                >
                  <UploadCloud size={18} className="mr-2" />
                  {preview && toPublish === 0 && preview.unreadable === 0
                    ? t('publish.actions.upToDate')
                    : t('publish.actions.publish', { count: preview ? toPublish : (album?.images.length ?? 0) })}
                </Button>
              </motion.div>
            </div>
          )}
        </>
      )}

      <PublishManagerModal />
    </div>
  );
}

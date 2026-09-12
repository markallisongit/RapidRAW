import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { motion } from 'framer-motion';
import { AlertTriangle, ArrowUpRight, Loader, Send, UploadCloud, X } from 'lucide-react';

import Button from '../../../ui/Button';
import Dropdown from '../../../ui/Dropdown';
import Text from '../../../ui/Text';
import { AlbumItem, Panel } from '../../../ui/AppProperties';
import {
  ExportPreset,
  ExportSettings,
  FILE_FORMATS,
  FileFormats,
  WatermarkAnchor,
} from '../../../ui/ExportImportProperties';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import { useExportSettings } from '../../../../hooks/useExportSettings';
import { useLibraryStore } from '../../../../store/useLibraryStore';
import { useSettingsStore } from '../../../../store/useSettingsStore';
import { DEFAULT_PANEL_WIDTH, useUIStore } from '../../../../store/useUIStore';
import PublishProgress from './PublishProgress';
import SmugMugAuthCard from './SmugMugAuthCard';
import { PublishPreview, PublishTarget, displayError, usePublishState } from './usePublishState';

const DESTINATION_ID = 'smugmug';

/** The extension-to-MIME mapping `ExportPipeline::mime` applies in the backend. */
const MIME_BY_EXTENSION: Record<string, string> = {
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  png: 'image/png',
  tif: 'image/tiff',
  tiff: 'image/tiff',
  webp: 'image/webp',
  jxl: 'image/jxl',
};

const QUALITY_FORMATS: string[] = [FileFormats.Jpeg, FileFormats.Webp, FileFormats.Jxl];

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

/** The same `ExportSettings` the Export panel builds from these values. */
const toExportSettings = (s: Omit<ExportPreset, 'id' | 'name'>): ExportSettings => ({
  filenameTemplate: s.filenameTemplate,
  jpegQuality: s.jpegQuality,
  keepMetadata: s.keepMetadata,
  preserveTimestamps: s.preserveTimestamps,
  preserveFolders: s.preserveFolders,
  destinationType: s.destinationType,
  subfolder: s.subfolder,
  resize: s.enableResize ? { mode: s.resizeMode, value: s.resizeValue, dontEnlarge: s.dontEnlarge } : null,
  stripGps: s.stripGps,
  exportMasks: s.exportMasks,
  watermark:
    s.enableWatermark && s.watermarkPath
      ? {
          path: s.watermarkPath,
          anchor: s.watermarkAnchor as WatermarkAnchor,
          scale: s.watermarkScale,
          spacing: s.watermarkSpacing,
          opacity: s.watermarkOpacity,
        }
      : null,
});

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

/**
 * The library's entry point: a button level with the bottom bar while closed,
 * the panel beside the grid while open. Desktop only — the keyring the
 * credentials live in has no Android backend.
 */
export function PublishDock() {
  const { t } = useTranslation();
  const { isPublishPanelVisible, activeView, setUI } = useUIStore(
    useShallow((state) => ({
      isPublishPanelVisible: state.isPublishPanelVisible,
      activeView: state.activeView,
      setUI: state.setUI,
    })),
  );
  const hasRoots = useLibraryStore((state) => state.rootPaths.length > 0);
  const isVisible = isPublishPanelVisible && hasRoots && activeView !== 'community';

  return (
    <div
      className={isVisible ? 'shrink-0 h-full ml-2' : 'shrink-0 flex flex-col justify-end'}
      style={isVisible ? { width: DEFAULT_PANEL_WIDTH } : undefined}
    >
      <PublishPanel isVisible={isVisible} onClose={() => setUI({ isPublishPanelVisible: false })} />
      {!isVisible && hasRoots && activeView !== 'community' && (
        <button
          className="ml-2 h-12 w-12 bg-bg-secondary rounded-lg flex items-center justify-center text-text-secondary hover:text-text-primary transition-colors"
          onClick={() => setUI({ isPublishPanelVisible: true })}
          data-tooltip={t('publish.panel.openPanel')}
        >
          <Send size={18} />
        </button>
      )}
    </div>
  );
}

interface PublishPanelProps {
  isVisible: boolean;
  onClose: () => void;
}

/**
 * Stays mounted while hidden so a running session keeps its progress: the
 * backend reports a session only through events, and has no way to ask what
 * one is doing after the fact.
 */
export default function PublishPanel({ isVisible, onClose }: PublishPanelProps) {
  const { t } = useTranslation();
  const api = usePublishState(DESTINATION_ID, isVisible);
  const { authStatus, authError, destination, session } = api;
  const destinationName = destination?.display_name ?? 'SmugMug';

  const appSettings = useSettingsStore((state) => state.appSettings);
  const setPanel = useUIStore((state) => state.setPanel);
  const { albumTree, activeAlbumId } = useLibraryStore(
    useShallow((state) => ({ albumTree: state.albumTree, activeAlbumId: state.activeAlbumId })),
  );

  // Export settings come from the Export panel's last-used preset, through
  // the same hook and defaults, rather than from a second settings UI.
  const { currentSettingsObject, handleApplyPreset } = useExportSettings();
  const lastUsedPreset = appSettings?.exportPresets?.find((p) => p.id === '__last_used__');
  useEffect(() => {
    if (lastUsedPreset) handleApplyPreset(lastUsedPreset);
  }, [lastUsedPreset, handleApplyPreset]);

  const albums = useMemo(() => flattenAlbums(albumTree), [albumTree]);
  const [selectedAlbumId, setSelectedAlbumId] = useState<string | null>(null);
  useEffect(() => {
    if (activeAlbumId && albums.some((a) => a.id === activeAlbumId)) setSelectedAlbumId(activeAlbumId);
  }, [activeAlbumId, albums]);
  const album = albums.find((a) => a.id === selectedAlbumId) ?? null;

  const format = FILE_FORMATS.find((f) => f.id === currentSettingsObject.fileFormat) ?? FILE_FORMATS[0];
  const outputFormat = format.extensions[0];
  const acceptedMimes = destination?.capabilities.accepted_mime_types ?? [];
  const isFormatAccepted = acceptedMimes.includes(MIME_BY_EXTENSION[outputFormat] ?? '');
  const acceptedFormatNames = FILE_FORMATS.filter((f) => acceptedMimes.includes(MIME_BY_EXTENSION[f.extensions[0]]))
    .map((f) => f.name)
    .join(', ');

  const target: PublishTarget | null = useMemo(
    () => (album ? { albumId: album.id, exportSettings: toExportSettings(currentSettingsObject), outputFormat } : null),
    [album, currentSettingsObject, outputFormat],
  );

  const [editing, setEditing] = useState<'configure' | 'authorise' | null>(null);
  useEffect(() => setEditing(null), [authStatus]);

  const isConnected = authStatus?.status === 'Connected';
  const [preview, setPreview] = useState<PublishPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [isPreviewing, setIsPreviewing] = useState(false);

  useEffect(() => {
    setPreview(null);
    setPreviewError(null);
    if (!isVisible || !isConnected || !target || !isFormatAccepted || session.phase !== 'idle') return;

    let isCurrent = true;
    setIsPreviewing(true);
    api
      .preview(target)
      .then((counts) => isCurrent && setPreview(counts))
      .catch((error) => isCurrent && setPreviewError(displayError(error, t('publish.errors.localFile'))))
      .finally(() => isCurrent && setIsPreviewing(false));
    return () => {
      isCurrent = false;
    };
  }, [isVisible, isConnected, target, album?.images, isFormatAccepted, session.phase, api.preview, t]);

  const resizeModeLabels: Record<string, string> = {
    longEdge: t('export.resize.modes.longEdge'),
    shortEdge: t('export.resize.modes.shortEdge'),
    width: t('export.resize.modes.width'),
    height: t('export.resize.modes.height'),
  };
  const settingsSummary = [
    format.name,
    QUALITY_FORMATS.includes(format.id) &&
      t('publish.settings.quality', { quality: currentSettingsObject.jpegQuality }),
    currentSettingsObject.enableResize
      ? `${resizeModeLabels[currentSettingsObject.resizeMode] ?? ''} ${currentSettingsObject.resizeValue} px`
      : t('publish.settings.fullSize'),
    currentSettingsObject.enableWatermark && currentSettingsObject.watermarkPath && t('publish.settings.watermark'),
  ]
    .filter(Boolean)
    .join(' · ');

  const toPublish = preview ? preview.new + preview.update : 0;
  const canPublish = isConnected && !!album && album.images.length > 0 && isFormatAccepted && !isPreviewing;

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
    if (authStatus.status === 'NotConfigured' || editing === 'configure') {
      return (
        <SmugMugAuthCard
          api={api}
          mode="configure"
          onCancel={authStatus.status !== 'NotConfigured' ? () => setEditing(null) : undefined}
        />
      );
    }
    if (authStatus.status === 'NotAuthorised' || editing === 'authorise') {
      return (
        <SmugMugAuthCard
          api={api}
          mode="authorise"
          onChangeKey={() => setEditing('configure')}
          onCancel={authStatus.status === 'Connected' ? () => setEditing(null) : undefined}
        />
      );
    }

    return (
      <>
        <Section title={t('publish.connected.heading')}>
          <Text color={TextColors.primary}>{t('publish.connected.account', { account: authStatus.account })}</Text>
          <div className="flex gap-4">
            <button
              className="text-sm text-text-secondary hover:text-text-primary"
              onClick={() => setEditing('authorise')}
            >
              {t('publish.connected.reconnect')}
            </button>
            <button
              className="text-sm text-text-secondary hover:text-text-primary"
              onClick={() => setEditing('configure')}
            >
              {t('publish.connected.changeKey')}
            </button>
          </div>
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
          <Text>{t('publish.settings.current')}</Text>
          <Text color={TextColors.primary} weight={TextWeights.medium}>
            {settingsSummary}
          </Text>
          {!isFormatAccepted && acceptedMimes.length > 0 && (
            <div className="flex items-start gap-2 bg-yellow-500/10 rounded-md p-3">
              <AlertTriangle size={16} className="shrink-0 mt-0.5 text-yellow-400" />
              <Text variant={TextVariants.small} color={TextColors.primary}>
                {t('publish.settings.unsupportedFormat', {
                  destination: destinationName,
                  formats: acceptedFormatNames,
                })}
              </Text>
            </div>
          )}
          <button
            className="flex items-center gap-1 text-sm text-accent hover:underline"
            onClick={() => setPanel(Panel.Export)}
          >
            {t('publish.settings.openExport')}
            <ArrowUpRight size={14} />
          </button>
        </Section>

        {album && album.images.length > 0 && isFormatAccepted && (
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
                      update: preview.update,
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
    <div className={isVisible ? 'h-full bg-bg-secondary rounded-lg flex flex-col' : 'hidden'}>
      <div className="p-3 flex justify-between items-center shrink-0 border-b border-surface">
        <Text variant={TextVariants.title} className="flex items-center gap-2">
          <Send size={18} /> {t('publish.panel.title')}
        </Text>
        <button
          className="p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-surface"
          onClick={onClose}
          data-tooltip={t('publish.panel.close')}
        >
          <X size={18} />
        </button>
      </div>

      {session.phase !== 'idle' ? (
        <PublishProgress api={api} destinationName={destinationName} />
      ) : (
        <>
          <div className="grow overflow-y-auto p-3 space-y-8">{renderBody()}</div>
          {isConnected && editing === null && (
            <div className="p-3 border-t border-surface shrink-0">
              <motion.div
                whileTap={canPublish ? { scale: 0.98 } : undefined}
                transition={{ type: 'spring', stiffness: 400, damping: 17 }}
                className="w-full"
              >
                <Button
                  className="rounded-md h-11 w-full flex items-center text-md font-bold! justify-center"
                  disabled={!canPublish}
                  onClick={() => target && api.publish(target)}
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
    </div>
  );
}

import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { motion } from 'framer-motion';
import { Loader, UploadCloud } from 'lucide-react';

import Button from '../../../ui/Button';
import { ExportPreset } from '../../../ui/ExportImportProperties';
import Text from '../../../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import { describeOutput } from './output';
import { LinkInfo, LinkPreview, PublishPreview, displayError, isPresetMissing } from './usePublishState';

/** What publishing would do, e.g. "4 to update · 38 unchanged". Empty for an album with no photos. */
export const describeCounts = (preview: PublishPreview, t: TFunction): string =>
  [
    preview.new > 0 && t('publish.summary.new', { count: preview.new }),
    preview.update > 0 && t('publish.summary.update', { count: preview.update }),
    preview.settings_changed > 0 && t('publish.summary.settings', { count: preview.settings_changed }),
    preview.skip > 0 && t('publish.summary.unchanged', { count: preview.skip }),
  ]
    .filter(Boolean)
    .join(' · ');

/** The preview's error, worded for the panel. */
export const previewError = (error: unknown, t: TFunction): string =>
  isPresetMissing(error) ? t('publish.errors.presetMissing') : displayError(error, t('publish.errors.localFile'));

/** Nothing to upload and nothing to report as failed, so publishing would do nothing. */
export const isPreviewUpToDate = (preview: PublishPreview) =>
  photosToPublish(preview) === 0 && preview.unreadable === 0;

/** Photos a publish would upload if settings-only changes are republished. */
export const photosToPublish = (preview: PublishPreview) => preview.new + preview.update + preview.settings_changed;

interface PublishSummaryProps {
  link: LinkInfo;
  entry: LinkPreview | undefined;
  preset: ExportPreset;
  destinationId: string;
  destinationName: string;
  isStarting: boolean;
  onPublish: () => void;
}

export default function PublishSummary({
  link,
  entry,
  preset,
  destinationId,
  destinationName,
  isStarting,
  onPublish,
}: PublishSummaryProps) {
  const { t } = useTranslation();
  const preview = entry?.preview ?? null;
  const isChecking = !preview && (entry?.isChecking ?? true);
  const error = entry && !entry.isChecking && entry.error ? previewError(entry.error, t) : null;
  const toPublish = preview ? photosToPublish(preview) : 0;
  const isUpToDate = preview !== null && isPreviewUpToDate(preview);
  const canPublish = !link.broken && preview !== null && !isUpToDate && !isStarting && !error;

  const renderStatus = () => {
    if (link.broken) {
      return (
        <Text variant={TextVariants.small} color={TextColors.error}>
          {t('publish.summary.broken', { destination: destinationName, context: destinationId })}
        </Text>
      );
    }
    if (error) {
      return (
        <Text variant={TextVariants.small} color={TextColors.error}>
          {t('publish.summary.failed', { error })}
        </Text>
      );
    }
    if (isChecking || !preview) {
      return (
        <Text variant={TextVariants.small} className="flex items-center gap-2 italic">
          <Loader size={12} className="animate-spin" /> {t('publish.summary.checking')}
        </Text>
      );
    }
    const counts = describeCounts(preview, t);
    return (
      <>
        <Text variant={TextVariants.small} color={TextColors.primary}>
          {counts || t('publish.summary.empty')}
        </Text>
        {preview.unreadable > 0 && (
          <Text variant={TextVariants.small} color={TextColors.error}>
            {t('publish.summary.unreadable', { count: preview.unreadable })}
          </Text>
        )}
      </>
    );
  };

  return (
    <div className="p-3 border-t border-surface shrink-0 space-y-1">
      <Text color={TextColors.primary} weight={TextWeights.medium} className="truncate">
        {link.remote_name
          ? t('publish.summary.mapping', {
              album: link.album_name ?? '',
              destination: destinationName,
              remote: link.remote_name,
            })
          : t('publish.summary.mappingUnnamed', { album: link.album_name ?? '', destination: destinationName })}
      </Text>
      {renderStatus()}
      <Text variant={TextVariants.small} className="truncate">
        {t('publish.summary.preset', { name: preset.name, output: describeOutput(preset, t) })}
      </Text>

      <motion.div
        whileTap={canPublish ? { scale: 0.98 } : undefined}
        transition={{ type: 'spring', stiffness: 400, damping: 17 }}
        className="w-full pt-2"
      >
        <Button
          className="rounded-md h-11 w-full flex items-center text-md font-bold! justify-center"
          disabled={!canPublish}
          onClick={onPublish}
          size="lg"
        >
          {isStarting ? (
            <>
              <Loader size={18} className="animate-spin mr-2" /> {t('publish.actions.starting')}
            </>
          ) : (
            <>
              <UploadCloud size={18} className="mr-2" />
              {isUpToDate ? t('publish.actions.upToDate') : t('publish.actions.publish', { count: toPublish })}
            </>
          )}
        </Button>
      </motion.div>
    </div>
  );
}

interface SettingsChangeQuestionProps {
  /** Photos whose only change is the output settings; null when not asking. */
  count: number | null;
  onRepublish: () => void;
  onKeepExisting: () => void;
  onCancel: () => void;
}

/** Asked before any publish that would otherwise upload photos only because the output settings changed. */
export function SettingsChangeQuestion({ count, onRepublish, onKeepExisting, onCancel }: SettingsChangeQuestionProps) {
  const { t } = useTranslation();
  const [shownCount, setShownCount] = useState(count);
  const [show, setShow] = useState(false);

  // Mounts and animates as `ConfirmModal` does, keeping the count through the closing animation.
  useEffect(() => {
    if (count !== null) {
      setShownCount(count);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    }
    setShow(false);
    const timer = setTimeout(() => setShownCount(null), 300);
    return () => clearTimeout(timer);
  }, [count]);

  if (shownCount === null) return null;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // The app's shortcuts listen on the window, and must not act on the library behind the question.
    e.nativeEvent.stopImmediatePropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    }
  };

  const content = (
    <div
      className={`fixed inset-0 flex items-center justify-center z-[9999] p-6 bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
      onClick={onCancel}
    >
      <div
        aria-describedby="publish-settings-change-message"
        aria-labelledby="publish-settings-change-title"
        aria-modal="true"
        className={`bg-surface rounded-lg shadow-xl p-6 w-full max-w-md text-text-primary transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
        role="alertdialog"
      >
        <Text variant={TextVariants.title} id="publish-settings-change-title" className="mb-4">
          {t('publish.settingsChange.title')}
        </Text>
        <Text id="publish-settings-change-message" className="mb-6">
          {t('publish.settingsChange.message', { count: shownCount })}
        </Text>
        <div className="flex flex-wrap justify-end gap-3">
          <Button className="bg-bg-primary text-text-primary shadow-none" onClick={onCancel}>
            {t('publish.settingsChange.cancel')}
          </Button>
          <Button className="bg-bg-primary text-text-primary shadow-none" onClick={onKeepExisting}>
            {t('publish.settingsChange.keep')}
          </Button>
          <Button autoFocus onClick={onRepublish}>
            {t('publish.settingsChange.republish')}
          </Button>
        </div>
      </div>
    </div>
  );

  return createPortal(content, document.body);
}

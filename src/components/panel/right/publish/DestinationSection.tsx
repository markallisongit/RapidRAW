import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { AlertTriangle, ArrowUpRight, ChevronDown, Loader, Plus, Settings, UploadCloud } from 'lucide-react';

import Button from '../../../ui/Button';
import { ExportPreset } from '../../../ui/ExportImportProperties';
import Text from '../../../ui/Text';
import { TextColors, TextVariants } from '../../../../types/typography';
import LinkedAlbumRow from './LinkedAlbumRow';
import { formatSupport } from './output';
import { LinkInfo, ManagerSection, PublishStateApi, displayError } from './usePublishState';

const SECONDARY_BUTTON = 'bg-surface text-text-primary shadow-none';

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

interface DestinationSectionProps {
  api: PublishStateApi;
  destinationName: string;
  /** The destination's preset; null when none is chosen or it was deleted. */
  preset: ExportPreset | null;
  /** Whether link statuses can be checked: connected, with a preset the destination accepts. */
  canCheck: boolean;
  /** The album selected in Sources, when it is not linked. */
  unlinkedActiveAlbum: { id: string; name: string } | null;
  onOpenManager: (section: ManagerSection | null) => void;
  onLinkAlbum: (albumId: string | null) => void;
  onPublish: (albumId: string) => void;
  onRemoveDeleted: (links: LinkInfo[]) => void;
  onRelink: (link: LinkInfo) => void;
  onUnlink: (link: LinkInfo) => void;
}

export default function DestinationSection({
  api,
  destinationName,
  preset,
  canCheck,
  unlinkedActiveAlbum,
  onOpenManager,
  onLinkAlbum,
  onPublish,
  onRemoveDeleted,
  onRelink,
  onUnlink,
}: DestinationSectionProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(true);
  const { authStatus, authError, destination, settings, links, linksError, previews, selectedAlbumId } = api;
  const account = authStatus?.status === 'Connected' ? authStatus.account : null;
  const dot =
    authStatus === null
      ? 'bg-text-secondary/40'
      : authStatus.status === 'Connected'
        ? 'bg-green-400'
        : authStatus.status === 'NotAuthorised'
          ? 'bg-yellow-400'
          : 'bg-text-secondary/40';

  const renderOutputWarning = () => {
    const presetId = settings?.export_preset_id ?? null;
    let message: string | null = null;
    if (settings !== null && preset === null) {
      message =
        presetId === null
          ? t('publish.destination.noPreset', { destination: destinationName })
          : t('publish.destination.presetDeleted', { destination: destinationName });
    } else if (preset) {
      const support = formatSupport(destination, preset.fileFormat);
      if (!support.isAccepted) {
        message = t('publish.destination.unsupportedFormat', {
          destination: destinationName,
          formats: support.acceptedNames,
        });
      }
    }
    if (message === null) return null;
    return (
      <div className="space-y-2">
        <Warning>{message}</Warning>
        <button
          className="flex items-center gap-1 text-sm text-accent hover:underline"
          onClick={() => onOpenManager('output')}
        >
          {t('publish.destination.choosePreset')}
          <ArrowUpRight size={14} />
        </button>
      </div>
    );
  };

  const renderLinks = () => {
    if (links === null) {
      return linksError ? (
        <div className="space-y-2">
          <Text variant={TextVariants.small} color={TextColors.error}>
            {t('publish.links.failed', { error: displayError(linksError, t('publish.errors.localFile')) })}
          </Text>
          <Button className={SECONDARY_BUTTON} onClick={api.refreshLinks}>
            {t('publish.panel.retry')}
          </Button>
        </div>
      ) : (
        <Text variant={TextVariants.small} className="flex items-center gap-2 italic">
          <Loader size={12} className="animate-spin" /> {t('publish.links.loading')}
        </Text>
      );
    }

    // A link whose RapidRAW album was deleted can never be published again, so it is only offered for removal.
    const live = links.filter((link) => link.album_name !== null);
    const deleted = links.filter((link) => link.album_name === null);
    const removeDeleted = deleted.length > 0 && (
      <button
        className="w-full px-2 py-1.5 rounded-md text-left text-xs text-text-secondary hover:text-text-primary hover:bg-surface/60 transition-colors"
        onClick={() => onRemoveDeleted(deleted)}
      >
        {t('publish.links.deletedLinks', { count: deleted.length })} ·{' '}
        <span className="text-accent">{t('publish.links.removeDeleted')}</span>
      </button>
    );

    if (live.length === 0) {
      return (
        <div className="space-y-3">
          <Text>{t('publish.links.empty', { destination: destinationName, context: destination?.id })}</Text>
          <Button onClick={() => onLinkAlbum(null)}>
            <Plus size={16} />
            {t('publish.links.publishAlbum')}
          </Button>
          {removeDeleted}
        </div>
      );
    }

    return (
      <div className="space-y-0.5">
        {live.map((link) => (
          <LinkedAlbumRow
            canCheck={canCheck}
            destinationId={destination?.id ?? ''}
            destinationName={destinationName}
            entry={previews[link.album_id]}
            isSelected={link.album_id === selectedAlbumId}
            key={link.album_id}
            link={link}
            onPublish={() => onPublish(link.album_id)}
            onRelink={() => onRelink(link)}
            onSelect={() => api.selectAlbum(link.album_id)}
            onUnlink={() => onUnlink(link)}
          />
        ))}
        <button
          className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-text-secondary hover:text-text-primary hover:bg-surface/60 transition-colors"
          onClick={() => onLinkAlbum(null)}
        >
          <Plus size={14} />
          {t('publish.links.publishAlbum')}
        </button>
        {removeDeleted}
      </div>
    );
  };

  const renderBody = () => {
    if (authError) {
      return (
        <div className="space-y-3">
          <Text color={TextColors.error}>{displayError(authError, t('publish.errors.localFile'))}</Text>
          <Button className={SECONDARY_BUTTON} onClick={api.refreshAuth}>
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
          <Button onClick={() => onOpenManager(hasKey ? 'account' : 'apiKey')}>
            <Settings size={16} />
            {hasKey
              ? t('publish.prompt.connect', { destination: destinationName })
              : t('publish.prompt.setUp', { destination: destinationName })}
          </Button>
        </div>
      );
    }

    return (
      <div className="space-y-3">
        {renderOutputWarning()}
        {unlinkedActiveAlbum && links !== null && (
          <Button
            className={clsx(SECONDARY_BUTTON, 'w-full justify-start!')}
            onClick={() => onLinkAlbum(unlinkedActiveAlbum.id)}
          >
            <UploadCloud size={16} className="shrink-0" />
            <span className="truncate">{t('publish.links.publishThisAlbum', { name: unlinkedActiveAlbum.name })}</span>
          </Button>
        )}
        {renderLinks()}
      </div>
    );
  };

  return (
    <div className="space-y-2">
      <button
        aria-expanded={isOpen}
        className="w-full flex items-center gap-2 py-1 text-left min-w-0"
        onClick={() => setIsOpen((open) => !open)}
      >
        <ChevronDown
          size={16}
          className={clsx('shrink-0 text-text-secondary transition-transform', !isOpen && '-rotate-90')}
        />
        <Text variant={TextVariants.heading} className="truncate flex-1">
          {destinationName}
          {account && <span className="font-normal text-text-secondary"> · {account}</span>}
        </Text>
        <span className={clsx('w-2 h-2 rounded-full shrink-0', dot)} />
      </button>
      {isOpen && renderBody()}
    </div>
  );
}

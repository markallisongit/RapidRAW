import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { open } from '@tauri-apps/plugin-shell';
import clsx from 'clsx';
import {
  AlertTriangle,
  Check,
  Circle,
  CircleDashed,
  ExternalLink,
  Link2,
  Loader,
  MoreHorizontal,
  RefreshCw,
  Settings2,
  Unlink,
  UploadCloud,
} from 'lucide-react';

import Text from '../../../ui/Text';
import { OPTION_SEPARATOR } from '../../../ui/AppProperties';
import { TextColors, TextVariants } from '../../../../types/typography';
import { useContextMenu } from '../../../../context/ContextMenuContext';
import { describeCounts, isPreviewUpToDate, previewError } from './PublishSummary';
import { LinkInfo, LinkPreview } from './usePublishState';

interface LinkStatus {
  icon: typeof Circle;
  className: string;
  label: string;
  tooltip?: string;
}

/** The one status that matters most, with every count in the tooltip. `null` while nothing can be checked. */
const linkStatus = (
  link: LinkInfo,
  entry: LinkPreview | undefined,
  canCheck: boolean,
  destinationName: string,
  t: TFunction,
): LinkStatus | null => {
  if (link.broken) {
    return {
      icon: AlertTriangle,
      className: 'text-yellow-400',
      label: t('publish.links.status.broken', { destination: destinationName }),
    };
  }
  if (!canCheck) return null;
  if (entry && !entry.isChecking && entry.error) {
    return {
      icon: AlertTriangle,
      className: 'text-red-400',
      label: t('publish.links.status.failed'),
      tooltip: previewError(entry.error, t),
    };
  }

  const preview = entry?.preview;
  if (!preview) {
    return { icon: Loader, className: 'text-text-secondary animate-spin', label: t('publish.links.status.checking') };
  }
  const tooltip = describeCounts(preview, t) || undefined;
  if (link.last_published === null) {
    return {
      icon: CircleDashed,
      className: 'text-text-secondary',
      label: t('publish.links.status.notPublished'),
      tooltip,
    };
  }
  if (preview.update > 0) {
    return {
      icon: RefreshCw,
      className: 'text-accent',
      label: t('publish.links.status.changed', { count: preview.update }),
      tooltip,
    };
  }
  if (preview.new > 0) {
    return {
      icon: Circle,
      className: 'text-accent',
      label: t('publish.links.status.new', { count: preview.new }),
      tooltip,
    };
  }
  if (preview.settings_changed > 0) {
    return {
      icon: Settings2,
      className: 'text-yellow-400',
      label: t('publish.links.status.settings', { count: preview.settings_changed }),
      tooltip,
    };
  }
  return { icon: Check, className: 'text-green-400', label: t('publish.links.status.upToDate'), tooltip };
};

interface LinkedAlbumRowProps {
  link: LinkInfo;
  entry: LinkPreview | undefined;
  /** Whether previews can run: connected, with a usable preset. */
  canCheck: boolean;
  destinationId: string;
  destinationName: string;
  isSelected: boolean;
  onSelect: () => void;
  onPublish: () => void;
  onRelink: () => void;
  onUnlink: () => void;
}

export default function LinkedAlbumRow({
  link,
  entry,
  canCheck,
  destinationId,
  destinationName,
  isSelected,
  onSelect,
  onPublish,
  onRelink,
  onUnlink,
}: LinkedAlbumRowProps) {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const name = link.album_name ?? '';
  const status = linkStatus(link, entry, canCheck, destinationName, t);
  const StatusIcon = status?.icon;
  const showsRemoteName = link.remote_name !== null && link.remote_name !== link.album_name;

  const showMenu = (x: number, y: number) =>
    showContextMenu(x, y, [
      {
        icon: UploadCloud,
        label: t('publish.links.menu.publish'),
        disabled: link.broken || !canCheck || (!!entry?.preview && isPreviewUpToDate(entry.preview)),
        onClick: onPublish,
      },
      {
        icon: ExternalLink,
        label: t('publish.links.menu.open', { destination: destinationName }),
        disabled: link.web_url === null || link.broken,
        onClick: () => link.web_url && open(link.web_url),
      },
      { type: OPTION_SEPARATOR },
      { icon: Link2, label: t('publish.links.menu.relink', { context: destinationId }), onClick: onRelink },
      { icon: Unlink, label: t('publish.links.menu.unlink'), isDestructive: true, onClick: onUnlink },
    ]);

  return (
    <div
      aria-current={isSelected}
      className={clsx(
        'group flex items-center gap-2 pl-2 pr-1 py-1.5 rounded-md cursor-pointer transition-colors',
        isSelected ? 'bg-surface' : 'hover:bg-surface/60',
      )}
      onClick={onSelect}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onSelect();
        showMenu(e.clientX, e.clientY);
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect();
        }
      }}
      role="button"
      tabIndex={0}
    >
      <div className="min-w-0 flex-1">
        <Text color={TextColors.primary} className="truncate">
          {link.album_path.length > 0 && <span className="text-text-secondary">{link.album_path.join(' › ')} › </span>}
          {name}
        </Text>
        {showsRemoteName && (
          <Text variant={TextVariants.small} className="truncate">
            {link.remote_name}
          </Text>
        )}
      </div>

      {status && StatusIcon && (
        <Text
          as="span"
          variant={TextVariants.small}
          className="flex items-center gap-1 shrink-0 max-w-[45%]"
          data-tooltip={status.tooltip}
        >
          <StatusIcon size={12} className={clsx('shrink-0', status.className)} />
          <span className="truncate">{status.label}</span>
        </Text>
      )}

      <button
        aria-label={t('publish.links.actions', { name })}
        className="p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-bg-primary opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity shrink-0"
        onClick={(e) => {
          e.stopPropagation();
          onSelect();
          const rect = e.currentTarget.getBoundingClientRect();
          showMenu(rect.left, rect.bottom);
        }}
      >
        <MoreHorizontal size={14} />
      </button>
    </div>
  );
}

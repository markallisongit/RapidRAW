import { Fragment, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { Album as AlbumIcon, ChevronRight, Folder, Loader } from 'lucide-react';

import Button from '../../../ui/Button';
import Text from '../../../ui/Text';
import { TextColors, TextVariants } from '../../../../types/typography';
import { LinkInfo, PublishStateApi, RemoteNode, displayError } from './usePublishState';

interface RemoteAlbumBrowserProps {
  api: PublishStateApi;
  destinationName: string;
  links: LinkInfo[];
  selected: RemoteNode | null;
  onSelect: (node: RemoteNode | null) => void;
}

/** The destination's albums, a folder at a time. Albums already linked are shown, but cannot be chosen. */
export default function RemoteAlbumBrowser({
  api,
  destinationName,
  links,
  selected,
  onSelect,
}: RemoteAlbumBrowserProps) {
  const { t } = useTranslation();
  const { listRemote } = api;
  const context = api.destination?.id;
  const [trail, setTrail] = useState<Array<{ id: string | null; name: string }>>([{ id: null, name: destinationName }]);
  const [nodes, setNodes] = useState<RemoteNode[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const parent = trail[trail.length - 1].id;

  useEffect(() => {
    let isCurrent = true;
    setNodes(null);
    setError(null);
    listRemote(parent)
      .then((next) => isCurrent && setNodes(next))
      .catch((e) => isCurrent && setError(e));
    return () => {
      isCurrent = false;
    };
  }, [listRemote, parent, attempt]);

  const linkByRemote = useMemo(() => new Map(links.map((link) => [link.remote_uri, link])), [links]);

  const openFolder = (node: RemoteNode) => {
    onSelect(null);
    setTrail((current) => [...current, { id: node.id, name: node.name }]);
  };

  const renderNodes = () => {
    if (error) {
      return (
        <div className="space-y-2 p-2">
          <Text variant={TextVariants.small} color={TextColors.error}>
            {t('publish.browser.failed', { error: displayError(error, t('publish.errors.localFile')), context })}
          </Text>
          <Button className="bg-surface text-text-primary shadow-none" onClick={() => setAttempt((n) => n + 1)}>
            {t('publish.panel.retry')}
          </Button>
        </div>
      );
    }
    if (nodes === null) {
      return (
        <Text variant={TextVariants.small} className="flex items-center gap-2 italic p-2">
          <Loader size={12} className="animate-spin" /> {t('publish.browser.loading', { context })}
        </Text>
      );
    }
    if (nodes.length === 0) {
      return (
        <Text variant={TextVariants.small} className="p-2">
          {t('publish.browser.empty')}
        </Text>
      );
    }

    return nodes.map((node) => {
      if (node.kind === 'Folder') {
        return (
          <button
            className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-surface text-left"
            key={node.id}
            onClick={() => openFolder(node)}
          >
            <Folder size={14} className="shrink-0 text-text-secondary" />
            <Text color={TextColors.primary} className="truncate flex-1">
              {node.name}
            </Text>
            <ChevronRight size={14} className="shrink-0 text-text-secondary" />
          </button>
        );
      }

      const linked = node.container ? linkByRemote.get(node.container) : undefined;
      const isSelected = selected?.id === node.id;
      return (
        <button
          aria-pressed={isSelected}
          className={clsx(
            'w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left transition-colors',
            'disabled:opacity-50 disabled:cursor-not-allowed',
            isSelected ? 'bg-accent/20 ring-1 ring-accent' : 'hover:bg-surface disabled:hover:bg-transparent',
          )}
          disabled={linked !== undefined}
          key={node.id}
          onClick={() => onSelect(node)}
        >
          <AlbumIcon size={14} className="shrink-0 text-text-secondary" />
          <div className="min-w-0 flex-1">
            <Text color={TextColors.primary} className="truncate">
              {node.name}
            </Text>
            {linked && (
              <Text variant={TextVariants.small} className="truncate">
                {linked.album_name === null
                  ? t('publish.browser.linkedToDeleted')
                  : t('publish.browser.linkedTo', { name: linked.album_name })}
              </Text>
            )}
          </div>
        </button>
      );
    });
  };

  return (
    <div className="space-y-2">
      <nav className="flex flex-wrap items-center gap-1 min-w-0">
        {trail.map((crumb, index) => {
          const isLast = index === trail.length - 1;
          return (
            <Fragment key={crumb.id ?? 'root'}>
              {index > 0 && <ChevronRight size={12} className="shrink-0 text-text-secondary" />}
              {isLast ? (
                <Text as="span" variant={TextVariants.small} color={TextColors.primary} className="truncate">
                  {crumb.name}
                </Text>
              ) : (
                <button
                  className="text-xs text-accent hover:underline truncate"
                  onClick={() => {
                    onSelect(null);
                    setTrail((current) => current.slice(0, index + 1));
                  }}
                >
                  {crumb.name}
                </button>
              )}
            </Fragment>
          );
        })}
      </nav>
      <div className="bg-bg-primary rounded-md p-1 space-y-0.5">{renderNodes()}</div>
    </div>
  );
}

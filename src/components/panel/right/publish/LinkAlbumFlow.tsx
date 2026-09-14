import { useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { Album as AlbumIcon, ArrowLeft, ArrowUpRight, Folder, Loader } from 'lucide-react';

import Button from '../../../ui/Button';
import Input from '../../../ui/Input';
import Text from '../../../ui/Text';
import { AlbumItem } from '../../../ui/AppProperties';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import RemoteAlbumBrowser from './RemoteAlbumBrowser';
import {
  ContainerPrivacy,
  ExistingMatch,
  LinkError,
  LinkInfo,
  LinkTarget,
  PublishStateApi,
  RemoteNode,
  displayError,
  isPresetMissing,
} from './usePublishState';

const SECONDARY_BUTTON = 'bg-surface text-text-primary shadow-none';

const isLinkError = (error: unknown): error is LinkError =>
  typeof error === 'object' && error !== null && 'kind' in error;

const findAlbumName = (items: AlbumItem[], albumId: string): string | null => {
  for (const item of items) {
    if (item.type === 'album' && item.id === albumId) return item.name;
    if (item.type === 'group') {
      const found = findAlbumName(item.children, albumId);
      if (found !== null) return found;
    }
  }
  return null;
};

const hasAlbums = (items: AlbumItem[]): boolean =>
  items.some((item) => item.type === 'album' || hasAlbums(item.children));

/** What linking to an existing remote album found in it, when there is something to say. */
type Existing =
  | { kind: 'matched'; match: ExistingMatch }
  | { kind: 'noMatch'; match: ExistingMatch }
  | { kind: 'noPreset' }
  | { kind: 'failed'; error: unknown };

function AlbumChoices({
  items,
  depth,
  linkedIds,
  onChoose,
}: {
  items: AlbumItem[];
  depth: number;
  linkedIds: Set<string>;
  onChoose: (albumId: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      {items.map((item) =>
        item.type === 'group' ? (
          <div key={item.id}>
            <div className="flex items-center gap-2 px-2 py-1.5" style={{ paddingLeft: `${0.5 + depth}rem` }}>
              <Folder size={14} className="shrink-0 text-text-secondary" />
              <Text variant={TextVariants.small} className="truncate">
                {item.name}
              </Text>
            </div>
            <AlbumChoices items={item.children} depth={depth + 1} linkedIds={linkedIds} onChoose={onChoose} />
          </div>
        ) : (
          <button
            className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left hover:bg-surface disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-transparent"
            disabled={linkedIds.has(item.id)}
            key={item.id}
            onClick={() => onChoose(item.id)}
            style={{ paddingLeft: `${0.5 + depth}rem` }}
          >
            <AlbumIcon size={14} className="shrink-0 text-text-secondary" />
            <Text color={TextColors.primary} className="truncate flex-1">
              {item.name}
            </Text>
            {linkedIds.has(item.id) && <Text variant={TextVariants.small}>{t('publish.link.linked')}</Text>}
          </button>
        ),
      )}
    </>
  );
}

interface LinkAlbumFlowProps {
  api: PublishStateApi;
  destinationName: string;
  albumTree: AlbumItem[];
  links: LinkInfo[];
  /** Skips choosing the RapidRAW album. */
  initialAlbumId: string | null;
  /** Linking an album that is linked already, to a different remote album. */
  isRelink: boolean;
  privacy: ContainerPrivacy;
  /** The destination's output preset, which names the photos it publishes. */
  presetName: string | null;
  onChangePrivacy: () => void;
  onChooseOutput: () => void;
  onDone: () => void;
  onCancel: () => void;
}

/**
 * Links a RapidRAW album to a remote album: a new one, or one that already
 * exists. Nothing uploads; the album is published from the panel afterwards.
 * Linking to one that exists asks before closing whether photos already in it
 * count as published, so they are not uploaded twice.
 */
export default function LinkAlbumFlow({
  api,
  destinationName,
  albumTree,
  links,
  initialAlbumId,
  isRelink,
  privacy,
  presetName,
  onChangePrivacy,
  onChooseOutput,
  onDone,
  onCancel,
}: LinkAlbumFlowProps) {
  const { t } = useTranslation();
  /** Selects the destination's own word for an album. */
  const context = api.destination?.id;
  const [albumId, setAlbumId] = useState(initialAlbumId);
  const [mode, setMode] = useState<'create' | 'existing'>('create');
  const [name, setName] = useState(() => (initialAlbumId ? (findAlbumName(albumTree, initialAlbumId) ?? '') : ''));
  const [remote, setRemote] = useState<RemoteNode | null>(null);
  const [isLinking, setIsLinking] = useState(false);
  const [isChecking, setIsChecking] = useState(false);
  const [error, setError] = useState<unknown>(null);
  /** Set once linked: the link stands whatever is answered here. */
  const [existing, setExisting] = useState<{ remoteName: string; found: Existing } | null>(null);
  const [isAdopting, setIsAdopting] = useState(false);
  const [adoptError, setAdoptError] = useState<unknown>(null);

  const linkedIds = useMemo(() => new Set(links.map((link) => link.album_id)), [links]);
  const albumName = albumId ? findAlbumName(albumTree, albumId) : null;

  const chooseAlbum = (id: string) => {
    setAlbumId(id);
    setName(findAlbumName(albumTree, id) ?? '');
    setError(null);
  };

  const back = () => {
    if (albumId === null || initialAlbumId !== null) {
      onCancel();
      return;
    }
    setAlbumId(null);
    setRemote(null);
    setError(null);
  };

  /** `null` when there is nothing to ask or explain: an empty album, or a destination that cannot list one. */
  const checkExisting = async (id: string): Promise<Existing | null> => {
    setIsChecking(true);
    try {
      const match = await api.matchExisting(id);
      if (match.matched > 0) return { kind: 'matched', match };
      return match.remote_photos > 0 ? { kind: 'noMatch', match } : null;
    } catch (e) {
      return isPresetMissing(e) ? { kind: 'noPreset' } : { kind: 'failed', error: e };
    }
  };

  const link = async (target: LinkTarget) => {
    if (!albumId) return;
    setIsLinking(true);
    setError(null);
    let info: LinkInfo;
    try {
      info = await api.linkAlbum(albumId, target);
    } catch (e) {
      setError(e);
      setIsLinking(false);
      return;
    }
    // A new album has nothing in it to adopt.
    const found = target.kind === 'Existing' ? await checkExisting(albumId) : null;
    if (found === null) {
      onDone();
      return;
    }
    setExisting({ remoteName: info.remote_name ?? '', found });
    setIsChecking(false);
    setIsLinking(false);
  };

  const adopt = async () => {
    if (!albumId) return;
    setIsAdopting(true);
    setAdoptError(null);
    try {
      await api.adoptExisting(albumId);
      onDone();
    } catch (e) {
      setAdoptError(e);
      setIsAdopting(false);
    }
  };

  const renderError = () => {
    if (!error) return null;
    if (isLinkError(error) && error.kind === 'AlreadyExists') {
      const container = error.remote.container;
      return (
        <div className="bg-yellow-500/10 rounded-md p-3 space-y-2">
          <Text variant={TextVariants.small} color={TextColors.primary}>
            {t('publish.link.alreadyExists', { name: error.remote.name, destination: destinationName, context })}
          </Text>
          {container && (
            <Button
              className={SECONDARY_BUTTON}
              disabled={isLinking}
              onClick={() => link({ kind: 'Existing', remote_uri: container })}
            >
              {isChecking ? t('publish.link.checking', { context }) : t('publish.link.linkInstead')}
            </Button>
          )}
        </div>
      );
    }
    const message = !isLinkError(error)
      ? displayError(error, t('publish.errors.localFile'))
      : error.kind === 'AlreadyLinked'
        ? error.album_name === null
          ? t('publish.link.alreadyLinkedDeleted', { context })
          : t('publish.link.alreadyLinked', { name: error.album_name, context })
        : displayError(error.kind === 'Failed' ? error.message : error, t('publish.errors.localFile'));
    return (
      <Text variant={TextVariants.small} color={TextColors.error}>
        {message}
      </Text>
    );
  };

  const renderExisting = ({ remoteName, found }: { remoteName: string; found: Existing }) => {
    if (found.kind === 'matched') {
      const count = found.match.matched;
      return (
        <div className="space-y-3">
          <Text color={TextColors.primary} weight={TextWeights.medium}>
            {t('publish.link.existing.title', { count, name: remoteName })}
          </Text>
          <Text variant={TextVariants.small}>{t('publish.link.existing.message', { count })}</Text>
          {adoptError !== null && (
            <Text variant={TextVariants.small} color={TextColors.error}>
              {isPresetMissing(adoptError)
                ? t('publish.link.existing.noPreset', { context })
                : displayError(adoptError, t('publish.errors.localFile'))}
            </Text>
          )}
          <Button className="w-full" disabled={isAdopting} onClick={adopt}>
            {isAdopting && <Loader size={16} className="animate-spin" />}
            {isAdopting ? t('publish.link.existing.adopting') : t('publish.link.existing.adopt', { count })}
          </Button>
          <Button className={clsx(SECONDARY_BUTTON, 'w-full')} disabled={isAdopting} onClick={onDone}>
            {t('publish.link.existing.uploadAgain', { count })}
          </Button>
        </div>
      );
    }

    return (
      <div className="space-y-3">
        {found.kind === 'noMatch' && (
          <Text variant={TextVariants.small} color={TextColors.primary}>
            {t('publish.link.existing.noMatch', { count: found.match.remote_photos, name: remoteName })}
            {found.match.example_file_name !== null && presetName !== null && (
              <>
                {' '}
                <Trans
                  i18nKey="publish.link.existing.naming"
                  values={{ example: found.match.example_file_name, preset: presetName }}
                  components={{ code: <code className="font-mono" /> }}
                />
              </>
            )}
          </Text>
        )}
        {found.kind === 'noPreset' && (
          <div className="space-y-1">
            <Text variant={TextVariants.small} color={TextColors.primary}>
              {t('publish.link.existing.noPreset', { context })}
            </Text>
            <button className="flex items-center gap-1 text-sm text-accent hover:underline" onClick={onChooseOutput}>
              {t('publish.link.existing.choosePreset')}
              <ArrowUpRight size={14} />
            </button>
          </div>
        )}
        {found.kind === 'failed' && (
          <Text variant={TextVariants.small} color={TextColors.error}>
            {t('publish.link.existing.failed', {
              name: remoteName,
              error: displayError(found.error, t('publish.errors.localFile')),
            })}
          </Text>
        )}
        <Button className="w-full" onClick={onDone}>
          {t('publish.link.existing.done')}
        </Button>
      </div>
    );
  };

  const renderTarget = () => {
    const trimmed = name.trim();
    return (
      <div className="space-y-4">
        <Text color={TextColors.primary} weight={TextWeights.medium}>
          {t('publish.link.chooseTarget', { name: albumName ?? '' })}
        </Text>

        <div role="radiogroup" className="grid grid-cols-2 gap-1 bg-bg-primary rounded-md p-1">
          {(['create', 'existing'] as const).map((choice) => (
            <button
              aria-checked={mode === choice}
              className={clsx(
                'px-2 py-1.5 rounded text-xs font-medium transition-colors',
                mode === choice ? 'bg-accent text-button-text' : 'text-text-secondary hover:text-text-primary',
              )}
              key={choice}
              onClick={() => {
                setMode(choice);
                setError(null);
              }}
              role="radio"
            >
              {choice === 'create'
                ? t('publish.link.createNew', { destination: destinationName, context })
                : t('publish.link.linkExisting', { context })}
            </button>
          ))}
        </div>

        {mode === 'create' ? (
          <div className="space-y-3">
            <div className="space-y-1">
              <label htmlFor="publish-new-album-name">
                <Text as="span" variant={TextVariants.label} className="block">
                  {t('publish.link.nameLabel')}
                </Text>
              </label>
              <Input
                id="publish-new-album-name"
                onChange={(e) => setName(e.target.value)}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === 'Enter' && trimmed) link({ kind: 'CreateNew', name: trimmed });
                }}
                value={name}
              />
            </div>
            <div className="space-y-1">
              <Text variant={TextVariants.small} color={TextColors.primary}>
                <Trans
                  context={context}
                  i18nKey="publish.link.privacy"
                  values={{ privacy: t(`publish.manager.newAlbums.privacy.${privacy}`) }}
                  components={{ strong: <strong /> }}
                />
              </Text>
              <button className="flex items-center gap-1 text-sm text-accent hover:underline" onClick={onChangePrivacy}>
                {t('publish.link.changePrivacy')}
                <ArrowUpRight size={14} />
              </button>
            </div>
            {renderError()}
            <Button
              className="w-full"
              disabled={!trimmed || isLinking}
              onClick={() => link({ kind: 'CreateNew', name: trimmed })}
            >
              {isLinking && <Loader size={16} className="animate-spin" />}
              {isLinking ? t('publish.link.creating') : t('publish.link.create')}
            </Button>
          </div>
        ) : (
          <div className="space-y-3">
            <RemoteAlbumBrowser
              api={api}
              destinationName={destinationName}
              links={links}
              onSelect={(node) => {
                setRemote(node);
                setError(null);
              }}
              selected={remote}
            />
            {renderError()}
            <Button
              className="w-full"
              disabled={!remote?.container || isLinking}
              onClick={() => remote?.container && link({ kind: 'Existing', remote_uri: remote.container })}
            >
              {isLinking && <Loader size={16} className="animate-spin" />}
              {isChecking
                ? t('publish.link.checking', { context })
                : isLinking
                  ? t('publish.link.linking')
                  : remote
                    ? t('publish.link.linkTo', { name: remote.name })
                    : t('publish.link.chooseRemote', { context })}
            </Button>
          </div>
        )}

        <Text variant={TextVariants.small}>{t('publish.link.nothingUploads')}</Text>
      </div>
    );
  };

  return (
    <div className="grow overflow-y-auto p-3 space-y-4">
      <div className="flex items-center gap-2">
        <button
          aria-label={t('publish.link.back')}
          className="p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-surface"
          disabled={isAdopting}
          onClick={existing ? onDone : back}
        >
          <ArrowLeft size={16} />
        </button>
        <Text variant={TextVariants.heading} className="truncate">
          {isRelink ? t('publish.link.relinkTitle', { context }) : t('publish.link.title')}
        </Text>
      </div>

      {existing ? (
        renderExisting(existing)
      ) : albumId === null ? (
        <div className="space-y-2">
          <Text color={TextColors.primary} weight={TextWeights.medium}>
            {t('publish.link.chooseAlbum')}
          </Text>
          {hasAlbums(albumTree) ? (
            <div className="bg-bg-primary rounded-md p-1 space-y-0.5">
              <AlbumChoices items={albumTree} depth={0} linkedIds={linkedIds} onChoose={chooseAlbum} />
            </div>
          ) : (
            <Text>{t('publish.link.noAlbums')}</Text>
          )}
        </div>
      ) : (
        renderTarget()
      )}

      {!existing && (
        <Button className={clsx(SECONDARY_BUTTON, 'w-full')} onClick={onCancel}>
          {t('publish.link.cancel')}
        </Button>
      )}
    </div>
  );
}

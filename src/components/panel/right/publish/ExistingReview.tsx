import { useCallback, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Trans, useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { ArrowUpRight, Check, ImageOff, Loader } from 'lucide-react';

import Button from '../../../ui/Button';
import Text from '../../../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import {
  ExistingMatch,
  ExistingPair,
  MatchProgress,
  PublishStateApi,
  displayError,
  isPresetMissing,
} from './usePublishState';

const SECONDARY_BUTTON = 'bg-surface text-text-primary shadow-none';
const MODAL_SECONDARY_BUTTON = 'bg-bg-primary text-text-primary shadow-none';
const FOCUSABLE = 'button:not([disabled]), input:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

/** Thumbnails requested at once: a local one can mean rendering a raw file. */
const THUMBNAIL_LOADS = 3;

type CheckState =
  | { kind: 'checking'; progress: MatchProgress | null }
  | { kind: 'found'; match: ExistingMatch }
  | { kind: 'noPreset' }
  | { kind: 'failed'; error: unknown }
  | { kind: 'skipped'; recorded: number; skipped: number };

interface Check {
  albumId: string;
  remoteName: string;
  state: CheckState;
}

/** Any pair not matched by its exact publish name is seen before it is recorded. */
const needsReview = (match: ExistingMatch) => match.pairs.some((pair) => pair.confidence !== 'Exact');

/**
 * Checks a linked album's remote album for photos it already holds, and records the ones the user accepts.
 * Started from an event handler rather than an effect, so StrictMode never runs a check twice.
 */
export function useExistingCheck(api: PublishStateApi) {
  const { matchExisting, cancelMatch, adoptExisting } = api;
  const [check, setCheck] = useState<Check | null>(null);
  const [isReviewOpen, setIsReviewOpen] = useState(false);
  const [isAdopting, setIsAdopting] = useState(false);
  const [adoptError, setAdoptError] = useState<unknown>(null);
  /** Bumped by every start and cancel, so an answer that arrives late is ignored. */
  const runs = useRef(0);
  /** A check holds the backend's session slot, so only then may cancelling reach the backend. */
  const isChecking = useRef(false);

  const stopChecking = useCallback(() => {
    runs.current += 1;
    if (!isChecking.current) return;
    isChecking.current = false;
    cancelMatch().catch((error) => console.error('Publish cancel check:', error));
  }, [cancelMatch]);

  // Leaving the panel mid-check stops it.
  useEffect(() => stopChecking, [stopChecking]);

  /**
   * Resolves `false` when there is nothing to show: `quietWhenEmpty` and a remote album with no photos in it, or a
   * check cancelled meanwhile.
   */
  const start = useCallback(
    async (albumId: string, remoteName: string, quietWhenEmpty: boolean): Promise<boolean> => {
      const run = ++runs.current;
      const update = (state: CheckState) => {
        if (runs.current === run) setCheck((current) => current && { ...current, state });
      };
      setCheck({ albumId, remoteName, state: { kind: 'checking', progress: null } });
      setIsReviewOpen(false);
      setAdoptError(null);
      isChecking.current = true;
      try {
        const match = await matchExisting(albumId, (progress) => update({ kind: 'checking', progress }));
        if (runs.current !== run) return false;
        if (quietWhenEmpty && match.remote_photos === 0) {
          setCheck(null);
          return false;
        }
        update({ kind: 'found', match });
        setIsReviewOpen(needsReview(match));
        return true;
      } catch (error) {
        if (runs.current !== run) return false;
        update(isPresetMissing(error) ? { kind: 'noPreset' } : { kind: 'failed', error });
        return true;
      } finally {
        if (runs.current === run) isChecking.current = false;
      }
    },
    [matchExisting],
  );

  const cancel = useCallback(() => {
    stopChecking();
    setCheck(null);
    setIsReviewOpen(false);
  }, [stopChecking]);

  /** Resolves `true` when every pair was recorded, so there is nothing more to say. */
  const adopt = useCallback(
    async (pairs: ExistingPair[]): Promise<boolean> => {
      if (!check) return false;
      setIsAdopting(true);
      setAdoptError(null);
      try {
        const recorded = await adoptExisting(check.albumId, pairs);
        setIsReviewOpen(false);
        if (recorded === pairs.length) return true;
        setCheck({ ...check, state: { kind: 'skipped', recorded, skipped: pairs.length - recorded } });
        return false;
      } catch (error) {
        setAdoptError(error);
        return false;
      } finally {
        setIsAdopting(false);
      }
    },
    [check, adoptExisting],
  );

  return {
    check,
    isReviewOpen,
    isAdopting,
    adoptError,
    start,
    cancel,
    adopt,
    openReview: () => setIsReviewOpen(true),
    closeReview: () => setIsReviewOpen(false),
  };
}

export type ExistingCheckController = ReturnType<typeof useExistingCheck>;

interface ExistingCheckProps {
  api: PublishStateApi;
  controller: ExistingCheckController;
  /** The destination's output preset, which names the photos it publishes. */
  presetName: string | null;
  /** Checked straight after linking, which stands whatever is answered. */
  afterLinking: boolean;
  onChooseOutput: () => void;
  onDone: () => void;
}

/** What checking a remote album found, in the panel; pairs beyond exact names are reviewed in a modal. */
export default function ExistingCheck({
  api,
  controller,
  presetName,
  afterLinking,
  onChooseOutput,
  onDone,
}: ExistingCheckProps) {
  const { t } = useTranslation();
  const context = api.destination?.id;
  const { check, isAdopting, adoptError } = controller;
  if (!check) return null;
  const { remoteName: name, state } = check;

  const adoptErrorText = adoptError !== null && (
    <Text variant={TextVariants.small} color={TextColors.error}>
      {isPresetMissing(adoptError)
        ? t('publish.existing.noPreset', { context })
        : displayError(adoptError, t('publish.errors.localFile'))}
    </Text>
  );

  const doneButton = (
    <Button className="w-full" onClick={onDone}>
      {t('publish.existing.done')}
    </Button>
  );

  if (state.kind === 'checking') {
    return (
      <div className="space-y-3">
        <Text className="flex items-center gap-2">
          <Loader size={16} className="animate-spin shrink-0" />
          {state.progress
            ? t('publish.existing.comparing', { count: state.progress.total })
            : t('publish.existing.checking', { context })}
        </Text>
        {state.progress && (
          <div className="h-1 rounded-full bg-bg-primary overflow-hidden">
            <div
              className="h-full bg-accent transition-[width]"
              style={{ width: `${(100 * state.progress.checked) / Math.max(state.progress.total, 1)}%` }}
            />
          </div>
        )}
        <Button
          className={clsx(SECONDARY_BUTTON, 'w-full')}
          onClick={() => {
            controller.cancel();
            onDone();
          }}
        >
          {t('publish.existing.cancel')}
        </Button>
      </div>
    );
  }

  if (state.kind === 'skipped') {
    return (
      <div className="space-y-3">
        <Text variant={TextVariants.small} color={TextColors.primary}>
          {t('publish.existing.skipped', { count: state.skipped, recorded: state.recorded })}
        </Text>
        {doneButton}
      </div>
    );
  }

  if (state.kind === 'noPreset') {
    return (
      <div className="space-y-3">
        <div className="space-y-1">
          <Text variant={TextVariants.small} color={TextColors.primary}>
            {t('publish.existing.noPreset', { context })}
          </Text>
          <button className="flex items-center gap-1 text-sm text-accent hover:underline" onClick={onChooseOutput}>
            {t('publish.existing.choosePreset')}
            <ArrowUpRight size={14} />
          </button>
        </div>
        {doneButton}
      </div>
    );
  }

  if (state.kind === 'failed') {
    const error = displayError(state.error, t('publish.errors.localFile'));
    return (
      <div className="space-y-3">
        <Text variant={TextVariants.small} color={TextColors.error}>
          {afterLinking
            ? t('publish.existing.failedLinked', { name, error })
            : t('publish.existing.failed', { name, error })}
        </Text>
        {doneButton}
      </div>
    );
  }

  const { match } = state;
  const count = match.pairs.length;

  if (count > 0 && !needsReview(match)) {
    return (
      <div className="space-y-3">
        <Text color={TextColors.primary} weight={TextWeights.medium}>
          {t('publish.existing.exact.title', { count, name })}
        </Text>
        <Text variant={TextVariants.small}>{t('publish.existing.exact.message', { count })}</Text>
        {adoptErrorText}
        <Button
          className="w-full"
          disabled={isAdopting}
          onClick={() => controller.adopt(match.pairs).then((done) => done && onDone())}
        >
          {isAdopting && <Loader size={16} className="animate-spin" />}
          {isAdopting ? t('publish.existing.adopting') : t('publish.existing.exact.adopt', { count })}
        </Button>
        <Button className={clsx(SECONDARY_BUTTON, 'w-full')} disabled={isAdopting} onClick={onDone}>
          {t('publish.existing.exact.uploadAgain', { count })}
        </Button>
      </div>
    );
  }

  if (count > 0) {
    return (
      <div className="space-y-3">
        <Text color={TextColors.primary} weight={TextWeights.medium}>
          {t('publish.existing.review.title', { count, name })}
        </Text>
        <Text variant={TextVariants.small}>{t('publish.existing.review.message')}</Text>
        <Button className="w-full" onClick={controller.openReview}>
          {t('publish.existing.review.open', { count })}
        </Button>
        <Button className={clsx(SECONDARY_BUTTON, 'w-full')} onClick={onDone}>
          {t('publish.existing.review.uploadAgain')}
        </Button>
        <ExistingReview api={api} controller={controller} match={match} name={name} onDone={onDone} />
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <Text variant={TextVariants.small} color={TextColors.primary}>
        {match.remote_photos === 0 ? (
          t('publish.existing.empty', { name })
        ) : match.example_file_name === null ? (
          t('publish.existing.allRecorded', { name })
        ) : (
          <>
            {t('publish.existing.noMatch', { count: match.remote_photos, name })}{' '}
            {match.example_remote_name !== null ? (
              <Trans
                context={context}
                i18nKey="publish.existing.namingBoth"
                values={{ example: match.example_file_name, remote: match.example_remote_name }}
                components={{ code: <code className="font-mono break-all" /> }}
              />
            ) : (
              presetName !== null && (
                <Trans
                  i18nKey="publish.existing.naming"
                  values={{ example: match.example_file_name, preset: presetName }}
                  components={{ code: <code className="font-mono break-all" /> }}
                />
              )
            )}
          </>
        )}
      </Text>
      {doneButton}
    </div>
  );
}

/** Runs thumbnail requests a few at a time, in the order they were asked for. */
const waiting: Array<() => void> = [];
let loading = 0;
const loadInTurn = <T,>(task: () => Promise<T>): Promise<T> =>
  new Promise((resolve, reject) => {
    const run = () => {
      loading += 1;
      task()
        .then(resolve, reject)
        .finally(() => {
          loading -= 1;
          waiting.shift()?.();
        });
    };
    if (loading < THUMBNAIL_LOADS) run();
    else waiting.push(run);
  });

/** One request per thumbnail for as long as a review is open, however often its row renders. */
type ThumbnailCache = Map<string, Promise<string | null>>;

function Thumbnail({
  cache,
  cacheKey,
  load,
  alt,
}: {
  cache: ThumbnailCache;
  cacheKey: string | null;
  load: () => Promise<string | null>;
  alt: string;
}) {
  const { t } = useTranslation();
  const [src, setSrc] = useState<string | null | undefined>(undefined);
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    if (cacheKey === null) {
      setSrc(null);
      return;
    }
    let isCurrent = true;
    let request = cache.get(cacheKey);
    if (!request) {
      request = loadInTurn(() => loadRef.current()).catch((error) => {
        console.error('Publish thumbnail:', error);
        return null;
      });
      cache.set(cacheKey, request);
    }
    request.then((url) => isCurrent && setSrc(url));
    return () => {
      isCurrent = false;
    };
  }, [cache, cacheKey]);

  return (
    <div className="w-28 h-28 shrink-0 rounded-md bg-bg-primary flex items-center justify-center overflow-hidden">
      {src === undefined ? (
        <Loader size={16} className="animate-spin text-text-secondary" />
      ) : src === null ? (
        <span
          className="flex flex-col items-center gap-1 text-text-secondary"
          title={t('publish.existing.review.noThumbnail')}
        >
          <ImageOff size={18} />
        </span>
      ) : (
        <img alt={alt} className="max-w-full max-h-full object-contain" src={src} />
      )}
    </div>
  );
}

const baseName = (path: string) => {
  const [file, copy] = path.split('?vc=');
  const name = file.split(/[\\/]/).pop() ?? file;
  return copy ? `${name} (VC${copy.padStart(2, '0')})` : name;
};

interface ExistingReviewProps {
  api: PublishStateApi;
  controller: ExistingCheckController;
  match: ExistingMatch;
  name: string;
  onDone: () => void;
}

/** Every pair side by side before any is recorded: a wrong pair would overwrite a different photo on its next edit. */
function ExistingReview({ api, controller, match, name, onDone }: ExistingReviewProps) {
  const { t } = useTranslation();
  const destinationName = api.destination?.display_name ?? '';
  const { isReviewOpen, isAdopting, adoptError, closeReview } = controller;
  const { localThumbnail, remoteThumbnail } = api;
  const [ticked, setTicked] = useState<Set<string>>(
    () => new Set(match.pairs.filter((pair) => pair.confidence !== 'Possible').map((pair) => pair.path)),
  );
  const [show, setShow] = useState(false);
  const [cache] = useState<ThumbnailCache>(() => new Map());
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isReviewOpen) {
      setShow(false);
      return;
    }
    const timer = setTimeout(() => setShow(true), 10);
    dialogRef.current?.focus();
    return () => clearTimeout(timer);
  }, [isReviewOpen]);

  const loadLocal = useCallback((path: string) => localThumbnail(path), [localThumbnail]);
  const loadRemote = useCallback((url: string) => remoteThumbnail(url), [remoteThumbnail]);

  if (!isReviewOpen) return null;

  const chosen = match.pairs.filter((pair) => ticked.has(pair.path));
  const hasPossible = match.pairs.some((pair) => pair.confidence === 'Possible');

  const toggle = (path: string) =>
    setTicked((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // The app's shortcuts listen on the window, and must not act on the library behind the review.
    e.nativeEvent.stopImmediatePropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (!isAdopting) closeReview();
    } else if (e.key === 'Tab' && dialogRef.current) {
      const focusable = Array.from(dialogRef.current.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey && (document.activeElement === first || document.activeElement === dialogRef.current)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  const content = (
    <div
      className={clsx(
        'fixed inset-0 flex items-center justify-center z-[9999] p-6',
        'bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out',
        show ? 'opacity-100' : 'opacity-0',
      )}
    >
      <div
        aria-describedby="publish-existing-review-message"
        aria-labelledby="publish-existing-review-title"
        aria-modal="true"
        className={clsx(
          'bg-bg-secondary rounded-lg shadow-xl w-full max-w-2xl max-h-[90vh] flex flex-col overflow-hidden',
          'text-text-primary focus:outline-hidden transform transition-all duration-300 ease-out',
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4',
        )}
        onKeyDown={handleKeyDown}
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
      >
        <div className="p-4 shrink-0 border-b border-surface space-y-2">
          <Text variant={TextVariants.title} id="publish-existing-review-title">
            {t('publish.existing.review.title', { count: match.pairs.length, name })}
          </Text>
          <Text id="publish-existing-review-message" variant={TextVariants.small}>
            {t('publish.existing.review.message')}
            {hasPossible && ` ${t('publish.existing.review.possibleNote')}`}
          </Text>
        </div>

        <ul className="grow overflow-y-auto p-2 space-y-1">
          {match.pairs.map((pair) => {
            const isTicked = ticked.has(pair.path);
            const localName = baseName(pair.path);
            return (
              <li key={pair.path}>
                <button
                  aria-checked={isTicked}
                  aria-label={t('publish.existing.review.tick', { name: localName })}
                  className={clsx(
                    'w-full flex items-center gap-3 p-2 rounded-md text-left transition-colors',
                    isTicked ? 'bg-surface' : 'hover:bg-surface/60',
                  )}
                  disabled={isAdopting}
                  onClick={() => toggle(pair.path)}
                  role="checkbox"
                >
                  <span
                    className={clsx(
                      'w-4 h-4 shrink-0 rounded border flex items-center justify-center',
                      isTicked ? 'bg-accent border-accent text-button-text' : 'border-text-secondary',
                    )}
                  >
                    {isTicked && <Check size={12} strokeWidth={3} />}
                  </span>
                  <Thumbnail
                    alt={localName}
                    cache={cache}
                    cacheKey={`local:${pair.path}`}
                    load={() => loadLocal(pair.path)}
                  />
                  <Thumbnail
                    alt={pair.remote_file_name}
                    cache={cache}
                    cacheKey={pair.remote_thumbnail_url && `remote:${pair.remote_thumbnail_url}`}
                    load={() =>
                      pair.remote_thumbnail_url ? loadRemote(pair.remote_thumbnail_url) : Promise.resolve(null)
                    }
                  />
                  <span className="min-w-0 flex-1 space-y-1">
                    <Text variant={TextVariants.small} className="truncate">
                      {t('publish.existing.review.inRapidRaw')}: <span className="text-text-primary">{localName}</span>
                    </Text>
                    <Text variant={TextVariants.small} className="truncate">
                      {t('publish.existing.review.onDestination', { destination: destinationName })}:{' '}
                      <span className="text-text-primary">{pair.remote_file_name}</span>
                    </Text>
                    <Text
                      variant={TextVariants.small}
                      color={pair.confidence === 'Possible' ? TextColors.secondary : TextColors.primary}
                    >
                      {pair.reasons.map((reason) => t(`publish.existing.review.reason.${reason}`)).join(' · ')}
                    </Text>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>

        <div className="p-4 shrink-0 border-t border-surface space-y-3">
          {adoptError !== null && (
            <Text variant={TextVariants.small} color={TextColors.error}>
              {displayError(adoptError, t('publish.errors.localFile'))}
            </Text>
          )}
          <div className="flex flex-wrap justify-end gap-3">
            <Button className={MODAL_SECONDARY_BUTTON} disabled={isAdopting} onClick={closeReview}>
              {t('publish.existing.review.close')}
            </Button>
            <Button className={MODAL_SECONDARY_BUTTON} disabled={isAdopting} onClick={onDone}>
              {t('publish.existing.review.uploadAgain')}
            </Button>
            <Button
              disabled={isAdopting || chosen.length === 0}
              onClick={() => controller.adopt(chosen).then((done) => done && onDone())}
            >
              {isAdopting && <Loader size={16} className="animate-spin" />}
              {isAdopting
                ? t('publish.existing.adopting')
                : chosen.length === 0
                  ? t('publish.existing.review.adoptNone')
                  : t('publish.existing.review.adopt', { count: chosen.length })}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );

  return createPortal(content, document.body);
}

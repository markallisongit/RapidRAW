import { ComponentType, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { Loader } from 'lucide-react';

import Button from '../../../../ui/Button';
import { Panel } from '../../../../ui/AppProperties';
import { ExportPreset } from '../../../../ui/ExportImportProperties';
import Text from '../../../../ui/Text';
import { TextColors, TextVariants } from '../../../../../types/typography';
import { useSettingsStore } from '../../../../../store/useSettingsStore';
import { useUIStore } from '../../../../../store/useUIStore';
import {
  DestinationInfo,
  DestinationSettings,
  ManagerRequest,
  ManagerSection,
  PublishStateApi,
  SettingsImpact,
  displayError,
  usePublishCatalogue,
  usePublishManager,
  usePublishState,
} from '../usePublishState';
import NewAlbumsSection from './NewAlbumsSection';
import OutputSection from './OutputSection';
import SmugMugAccountSection from './SmugMugAccountSection';

export interface AccountSectionProps {
  api: PublishStateApi;
  requestedSection: ManagerSection | null;
}

/** Sign-in differs by destination; a second destination adds an entry here. */
const ACCOUNT_SECTIONS: Record<string, ComponentType<AccountSectionProps>> = {
  smugmug: SmugMugAccountSection,
};

const SECONDARY_BUTTON = 'bg-surface text-text-primary shadow-none';
const NO_PRESETS: ExportPreset[] = [];
const FOCUSABLE = 'button:not([disabled]), input:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function DestinationRow({
  destination,
  isSelected,
  onSelect,
}: {
  destination: DestinationInfo;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const { authStatus } = usePublishState(destination.id, true);

  const [status, dot] =
    authStatus === null
      ? [t('publish.manager.status.checking'), 'bg-text-secondary/40']
      : authStatus.status === 'Connected'
        ? [t('publish.manager.status.connected', { account: authStatus.account }), 'bg-green-400']
        : authStatus.status === 'NotAuthorised'
          ? [t('publish.manager.status.notConnected'), 'bg-yellow-400']
          : [t('publish.manager.status.notSetUp'), 'bg-text-secondary/40'];

  return (
    <button
      aria-current={isSelected}
      className={clsx(
        'w-full text-left px-3 py-2 rounded-md transition-colors',
        isSelected ? 'bg-surface' : 'hover:bg-surface/60',
      )}
      onClick={onSelect}
    >
      <Text color={TextColors.primary}>{destination.display_name}</Text>
      <Text variant={TextVariants.small} className="flex items-center gap-1.5 min-w-0">
        <span className={clsx('w-2 h-2 rounded-full shrink-0', dot)} />
        <span className="truncate">{status}</span>
      </Text>
    </button>
  );
}

function ImpactQuestion({
  impact,
  destinationName,
  onRepublish,
  onKeepExisting,
  onCancel,
}: {
  impact: SettingsImpact;
  destinationName: string;
  onRepublish: () => void;
  onKeepExisting: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="absolute inset-0 flex items-center justify-center bg-black/40 p-6">
      <div
        aria-describedby="publish-impact-message"
        aria-labelledby="publish-impact-title"
        className="bg-surface rounded-lg shadow-xl p-6 w-full max-w-md"
        role="alertdialog"
      >
        <Text variant={TextVariants.title} id="publish-impact-title" className="mb-4">
          {t('publish.manager.impact.title')}
        </Text>
        <Text id="publish-impact-message" className="mb-6">
          {t('publish.manager.impact.message', {
            count: impact.photos,
            destination: destinationName,
            albums: t('publish.manager.impact.albums', { count: impact.albums }),
          })}
        </Text>
        <div className="flex flex-wrap justify-end gap-3">
          <Button className="bg-bg-primary text-text-primary shadow-none" onClick={onCancel}>
            {t('publish.manager.cancel')}
          </Button>
          <Button className="bg-bg-primary text-text-primary shadow-none" onClick={onKeepExisting}>
            {t('publish.manager.impact.keep')}
          </Button>
          <Button autoFocus onClick={onRepublish}>
            {t('publish.manager.impact.republish')}
          </Button>
        </div>
      </div>
    </div>
  );
}

function ManagerDialog({ request, show }: { request: ManagerRequest; show: boolean }) {
  const { t } = useTranslation();
  const { closeManager } = usePublishManager();
  const setPanel = useUIStore((state) => state.setPanel);
  const presets = useSettingsStore((state) => state.appSettings?.exportPresets) ?? NO_PRESETS;
  const catalogue = usePublishCatalogue();

  const [selectedId, setSelectedId] = useState(request.destinationId);
  const [requestedSection, setRequestedSection] = useState(request.section);
  const api = usePublishState(selectedId, true);
  const { destination, refreshSettings, saveSettings, settingsImpact } = api;
  const destinationName = destination?.display_name ?? selectedId;
  const AccountSection = ACCOUNT_SECTIONS[selectedId];

  const [saved, setSaved] = useState<DestinationSettings | null>(null);
  const [draft, setDraft] = useState<DestinationSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [impact, setImpact] = useState<SettingsImpact | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  const [loadAttempt, setLoadAttempt] = useState(0);

  // Switching destination discards the other's unsaved edits.
  useEffect(() => {
    let isCurrent = true;
    setSaved(null);
    setDraft(null);
    setLoadError(null);
    setSaveError(null);
    refreshSettings()
      .then((settings) => {
        if (!isCurrent) return;
        setSaved(settings);
        setDraft(settings);
      })
      .catch((error) => isCurrent && setLoadError(displayError(error, t('publish.errors.localFile'))));
    return () => {
      isCurrent = false;
    };
  }, [refreshSettings, loadAttempt, t]);

  useEffect(() => dialogRef.current?.focus(), []);

  const isDirty =
    saved !== null &&
    draft !== null &&
    (draft.export_preset_id !== saved.export_preset_id || draft.new_album_privacy !== saved.new_album_privacy);

  const commit = async (keepExistingUploads: boolean) => {
    if (!draft) return;
    setImpact(null);
    setIsSaving(true);
    setSaveError(null);
    try {
      await saveSettings(draft, keepExistingUploads);
      closeManager();
    } catch (error) {
      setSaveError(displayError(error, t('publish.errors.localFile')));
      // Keeping existing uploads can fail after the settings saved, so find
      // out which of the two happened.
      await refreshSettings()
        .then(setSaved)
        .catch(() => {});
    } finally {
      setIsSaving(false);
    }
  };

  const save = async () => {
    if (!draft || !saved || !isDirty) return;
    const presetId = draft.export_preset_id;
    if (presetId !== null && presetId !== saved.export_preset_id) {
      setIsSaving(true);
      setSaveError(null);
      try {
        const next = await settingsImpact(presetId);
        if (next.photos > 0) {
          setImpact(next);
          return;
        }
      } catch (error) {
        setSaveError(displayError(error, t('publish.errors.localFile')));
        return;
      } finally {
        setIsSaving(false);
      }
    }
    await commit(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // The app's shortcuts listen on the window, and must not act on the
    // library behind the manager.
    e.nativeEvent.stopImmediatePropagation();

    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (impact) setImpact(null);
      else closeManager();
    } else if (e.key === 'Tab' && dialogRef.current) {
      // While the question is asked, only its buttons take focus.
      const scope = (impact && dialogRef.current.querySelector('[role="alertdialog"]')) || dialogRef.current;
      const focusable = Array.from(scope.querySelectorAll<HTMLElement>(FOCUSABLE));
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

  const renderSettings = () => {
    if (loadError) {
      return (
        <div className="space-y-3">
          <Text color={TextColors.error}>{loadError}</Text>
          <Button className={SECONDARY_BUTTON} onClick={() => setLoadAttempt((n) => n + 1)}>
            {t('publish.panel.retry')}
          </Button>
        </div>
      );
    }
    if (!draft || !destination) {
      return (
        <Text className="flex items-center gap-2 italic">
          <Loader size={14} className="animate-spin" /> {t('publish.manager.loading')}
        </Text>
      );
    }
    return (
      <>
        <OutputSection
          destination={destination}
          presets={presets}
          presetId={draft.export_preset_id}
          isRequested={requestedSection === 'output'}
          onChange={(presetId) => setDraft({ ...draft, export_preset_id: presetId })}
          onManagePresets={() => {
            closeManager();
            setPanel(Panel.Export);
          }}
        />
        <NewAlbumsSection
          destination={destination}
          privacy={draft.new_album_privacy}
          isRequested={requestedSection === 'newAlbums'}
          onChange={(privacy) => setDraft({ ...draft, new_album_privacy: privacy })}
        />
      </>
    );
  };

  return (
    <div
      className={clsx(
        'fixed inset-0 flex items-center justify-center z-[9999] p-6',
        'bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out',
        show ? 'opacity-100' : 'opacity-0',
      )}
    >
      <div
        aria-labelledby="publish-manager-title"
        aria-modal="true"
        className={clsx(
          'relative bg-bg-secondary rounded-lg shadow-xl w-full max-w-3xl h-[min(680px,90vh)]',
          'flex flex-col overflow-hidden text-text-primary focus:outline-hidden',
          'transform transition-all duration-300 ease-out',
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4',
        )}
        onKeyDown={handleKeyDown}
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
      >
        <div className="p-4 shrink-0 border-b border-surface">
          <Text variant={TextVariants.title} id="publish-manager-title">
            {t('publish.manager.title')}
          </Text>
        </div>

        <div className="flex grow min-h-0">
          <nav
            aria-label={t('publish.manager.destinations')}
            className="w-56 shrink-0 border-r border-surface p-2 space-y-1 overflow-y-auto"
          >
            {catalogue === null ? (
              <Text className="flex items-center gap-2 italic p-2">
                <Loader size={14} className="animate-spin" /> {t('publish.manager.loading')}
              </Text>
            ) : (
              catalogue.map((d) => (
                <DestinationRow
                  destination={d}
                  isSelected={d.id === selectedId}
                  key={d.id}
                  onSelect={() => {
                    setRequestedSection(null);
                    setSelectedId(d.id);
                  }}
                />
              ))
            )}
          </nav>

          <div className="grow flex flex-col min-w-0">
            <div className="grow overflow-y-auto p-4 space-y-4">
              {AccountSection && <AccountSection api={api} key={selectedId} requestedSection={requestedSection} />}
              {renderSettings()}
            </div>

            <div className="p-4 shrink-0 border-t border-surface flex items-center justify-end gap-3">
              {saveError && (
                <Text variant={TextVariants.small} color={TextColors.error} className="mr-auto">
                  {saveError}
                </Text>
              )}
              <Button className={SECONDARY_BUTTON} onClick={closeManager}>
                {t('publish.manager.cancel')}
              </Button>
              <Button disabled={!isDirty || isSaving} onClick={save}>
                {isSaving && <Loader size={16} className="animate-spin" />}
                {isSaving ? t('publish.manager.saving') : t('publish.manager.save')}
              </Button>
            </div>
          </div>
        </div>

        {impact && (
          <ImpactQuestion
            impact={impact}
            destinationName={destinationName}
            onCancel={() => setImpact(null)}
            onKeepExisting={() => commit(true)}
            onRepublish={() => commit(false)}
          />
        )}
      </div>
    </div>
  );
}

/**
 * The Publish Manager: every destination's account, output preset and
 * new-album privacy. Mounted by the Publish panel and opened through
 * `usePublishManager`, animating in and out as `ConfirmModal` does.
 */
export default function PublishManagerModal() {
  const { request } = usePublishManager();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  // Kept through the closing animation, and counted so each opening starts afresh.
  const [shown, setShown] = useState<{ request: ManagerRequest; opening: number } | null>(null);

  useEffect(() => {
    if (request) {
      setShown((current) => ({ request, opening: (current?.opening ?? 0) + 1 }));
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    }
    setShow(false);
    const timer = setTimeout(() => setIsMounted(false), 300);
    return () => clearTimeout(timer);
  }, [request]);

  if (!isMounted || !shown) return null;

  const content = <ManagerDialog key={shown.opening} request={shown.request} show={show} />;
  return typeof document !== 'undefined' ? createPortal(content, document.body) : content;
}

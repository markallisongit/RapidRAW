import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { open } from '@tauri-apps/plugin-shell';
import { ExternalLink, KeyRound, Loader, LogOut, RefreshCw, ShieldCheck } from 'lucide-react';

import ConfirmModal from '../../../../modals/ConfirmModal';
import Button from '../../../../ui/Button';
import Input from '../../../../ui/Input';
import Text from '../../../../ui/Text';
import { TextColors, TextVariants } from '../../../../../types/typography';
import { displayError } from '../usePublishState';
import ManagerSection from './ManagerSection';
import type { AccountSectionProps } from './PublishManagerModal';

const DEVELOPER_APPLY_URL = 'https://api.smugmug.com/api/developer/apply';

const SECONDARY_BUTTON = 'bg-surface text-text-primary shadow-none border border-border-color';
const LINK_BUTTON = 'text-sm text-text-secondary hover:text-text-primary';

type Busy = 'save' | 'connect' | 'finish' | 'disconnect';

function Step({ number, title, children }: { number: number; title: string; children: React.ReactNode }) {
  return (
    <div className="flex gap-3">
      <div className="w-6 h-6 shrink-0 rounded-full bg-bg-primary flex items-center justify-center">
        <Text variant={TextVariants.small} color={TextColors.primary}>
          {number}
        </Text>
      </div>
      <div className="flex-1 min-w-0 space-y-2">
        <Text color={TextColors.primary}>{title}</Text>
        {children}
      </div>
    </div>
  );
}

/**
 * SmugMug's Account and API key sections. Every action here is a round-trip
 * with SmugMug or the keyring, so it takes effect at once rather than waiting
 * for the manager's Save.
 */
export default function SmugMugAccountSection({ api, requestedSection }: AccountSectionProps) {
  const { t } = useTranslation();
  const { authStatus, challenge } = api;
  const [key, setKey] = useState('');
  const [secret, setSecret] = useState('');
  const [verifier, setVerifier] = useState('');
  const [isChangingKey, setIsChangingKey] = useState(false);
  const [isReconnecting, setIsReconnecting] = useState(false);
  const [isConfirmingDisconnect, setIsConfirmingDisconnect] = useState(false);
  const [busy, setBusy] = useState<Busy | null>(null);
  const [error, setError] = useState<{ section: 'account' | 'apiKey'; message: string } | null>(null);

  useEffect(() => {
    setIsChangingKey(false);
    setIsReconnecting(false);
    setVerifier('');
  }, [authStatus]);

  const run = async (action: Busy, section: 'account' | 'apiKey', work: () => Promise<void>) => {
    setBusy(action);
    setError(null);
    try {
      await work();
    } catch (err) {
      setError({ section, message: displayError(err, t('publish.errors.localFile')) });
    } finally {
      setBusy(null);
    }
  };

  const errorLine = (section: 'account' | 'apiKey') =>
    error?.section === section && (
      <Text variant={TextVariants.small} color={TextColors.error}>
        {error.message}
      </Text>
    );

  const isConfigured = authStatus !== null && authStatus.status !== 'NotConfigured';
  const account = authStatus?.status === 'Connected' ? authStatus.account : '';
  const showKeyForm = authStatus?.status === 'NotConfigured' || isChangingKey;
  const showSignIn = authStatus?.status === 'NotAuthorised' || isReconnecting;
  const canSaveKey = key.trim() !== '' && secret.trim() !== '' && busy === null;
  const canFinish = challenge !== null && verifier.trim() !== '' && busy === null;

  const renderAccount = () => {
    if (!authStatus) {
      return (
        <Text className="flex items-center gap-2 italic">
          <Loader size={14} className="animate-spin" /> {t('publish.panel.loading')}
        </Text>
      );
    }
    if (authStatus.status === 'NotConfigured') {
      return <Text>{t('publish.manager.account.needsKey')}</Text>;
    }
    if (showSignIn) {
      return (
        <>
          <Text>{t('publish.smugmug.authInstructions')}</Text>
          <Step number={1} title={t('publish.smugmug.authorise.stepOne')}>
            <Button
              className={challenge ? SECONDARY_BUTTON : undefined}
              disabled={busy !== null}
              onClick={() => run('connect', 'account', challenge ? api.reopenAuthPage : api.beginAuth)}
            >
              {busy === 'connect' ? <Loader size={16} className="animate-spin" /> : <ExternalLink size={16} />}
              {busy === 'connect'
                ? t('publish.smugmug.authorise.opening')
                : challenge
                  ? t('publish.smugmug.authorise.reopen')
                  : t('publish.smugmug.authorise.connect')}
            </Button>
          </Step>
          <Step number={2} title={t('publish.smugmug.authorise.stepTwo')}>
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (canFinish) run('finish', 'account', () => api.completeAuth(verifier.trim()));
              }}
            >
              <Input
                className="font-mono tracking-widest text-center w-32!"
                disabled={challenge === null}
                placeholder={t('publish.smugmug.authorise.verifierPlaceholder')}
                value={verifier}
                onChange={(e) => setVerifier(e.target.value)}
              />
              <Button type="submit" disabled={!canFinish}>
                {busy === 'finish' && <Loader size={16} className="animate-spin" />}
                {busy === 'finish' ? t('publish.smugmug.authorise.finishing') : t('publish.smugmug.authorise.finish')}
              </Button>
            </form>
            {challenge === null && (
              <Text variant={TextVariants.small}>{t('publish.smugmug.authorise.codeAfterConnect')}</Text>
            )}
          </Step>
          {errorLine('account')}
          {isReconnecting && (
            <button className={LINK_BUTTON} onClick={() => setIsReconnecting(false)}>
              {t('publish.manager.cancel')}
            </button>
          )}
        </>
      );
    }
    return (
      <>
        <Text color={TextColors.primary}>{t('publish.manager.status.connected', { account })}</Text>
        <div className="flex gap-2">
          <Button className={SECONDARY_BUTTON} disabled={busy !== null} onClick={() => setIsReconnecting(true)}>
            <RefreshCw size={16} />
            {t('publish.manager.account.reconnect')}
          </Button>
          <Button className={SECONDARY_BUTTON} disabled={busy !== null} onClick={() => setIsConfirmingDisconnect(true)}>
            {busy === 'disconnect' ? <Loader size={16} className="animate-spin" /> : <LogOut size={16} />}
            {t('publish.manager.account.disconnect')}
          </Button>
        </div>
        {errorLine('account')}
      </>
    );
  };

  const privacyNote = (
    <div className="flex items-start gap-2 bg-bg-primary rounded-md p-3">
      <ShieldCheck size={16} className="shrink-0 mt-0.5 text-text-secondary" />
      <Text variant={TextVariants.small}>{t('publish.smugmug.setup.privacy')}</Text>
    </div>
  );

  const renderApiKey = () => {
    if (!showKeyForm) {
      return (
        <>
          <div className="flex items-center gap-3">
            <Text color={TextColors.primary} className="font-mono tracking-widest">
              ••••••••
            </Text>
            <Button
              className={SECONDARY_BUTTON}
              disabled={busy !== null || !isConfigured}
              onClick={() => setIsChangingKey(true)}
            >
              {t('publish.manager.apiKey.change')}
            </Button>
          </div>
          {privacyNote}
        </>
      );
    }
    return (
      <>
        <Text>{t('publish.smugmug.setup.intro')}</Text>
        <div className="space-y-1">
          <button
            className="flex items-center gap-1.5 text-accent hover:underline text-sm"
            onClick={() => open(DEVELOPER_APPLY_URL)}
          >
            <ExternalLink size={14} />
            {t('publish.smugmug.setup.applyLink')}
          </button>
          <Text variant={TextVariants.small}>{t('publish.smugmug.setup.applyHint')}</Text>
        </div>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (canSaveKey) {
              run('save', 'apiKey', async () => {
                await api.setCredentials(key.trim(), secret.trim());
                setKey('');
                setSecret('');
              });
            }
          }}
        >
          <label className="block space-y-1">
            <Text as="span" variant={TextVariants.label} className="block">
              {t('publish.smugmug.setup.keyLabel')}
            </Text>
            <Input value={key} onChange={(e) => setKey(e.target.value)} />
          </label>
          <label className="block space-y-1">
            <Text as="span" variant={TextVariants.label} className="block">
              {t('publish.smugmug.setup.secretLabel')}
            </Text>
            <Input type="password" value={secret} onChange={(e) => setSecret(e.target.value)} />
          </label>
          {privacyNote}
          {errorLine('apiKey')}
          <div className="flex gap-2">
            <Button type="submit" disabled={!canSaveKey}>
              {busy === 'save' ? <Loader size={16} className="animate-spin" /> : <KeyRound size={16} />}
              {busy === 'save' ? t('publish.smugmug.setup.saving') : t('publish.smugmug.setup.save')}
            </Button>
            {isChangingKey && (
              <Button type="button" className={SECONDARY_BUTTON} onClick={() => setIsChangingKey(false)}>
                {t('publish.manager.cancel')}
              </Button>
            )}
          </div>
        </form>
      </>
    );
  };

  return (
    <>
      <ManagerSection title={t('publish.manager.account.heading')} isRequested={requestedSection === 'account'}>
        {renderAccount()}
      </ManagerSection>
      <ManagerSection title={t('publish.manager.apiKey.heading')} isRequested={requestedSection === 'apiKey'}>
        {renderApiKey()}
      </ManagerSection>
      <ConfirmModal
        isOpen={isConfirmingDisconnect}
        title={t('publish.manager.account.confirmDisconnect.title', { destination: api.destination?.display_name })}
        message={t('publish.manager.account.confirmDisconnect.message', {
          destination: api.destination?.display_name,
        })}
        confirmText={t('publish.manager.account.disconnect')}
        cancelText={t('publish.manager.cancel')}
        onClose={() => setIsConfirmingDisconnect(false)}
        onConfirm={() => run('disconnect', 'account', api.disconnect)}
      />
    </>
  );
}

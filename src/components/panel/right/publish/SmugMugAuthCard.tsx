import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { open } from '@tauri-apps/plugin-shell';
import { ExternalLink, KeyRound, Loader, ShieldCheck } from 'lucide-react';

import Button from '../../../ui/Button';
import Input from '../../../ui/Input';
import Text from '../../../ui/Text';
import { TextColors, TextVariants } from '../../../../types/typography';
import { PublishStateApi, displayError } from './usePublishState';

const DEVELOPER_APPLY_URL = 'https://api.smugmug.com/api/developer/apply';

interface SmugMugAuthCardProps {
  api: PublishStateApi;
  mode: 'configure' | 'authorise';
  /** Offered when the card was opened from a connected account. */
  onCancel?: () => void;
  /** Switches from authorising to entering a different key. */
  onChangeKey?: () => void;
}

function Step({ number, title, children }: { number: number; title: string; children: React.ReactNode }) {
  return (
    <div className="flex gap-3">
      <div className="w-6 h-6 shrink-0 rounded-full bg-surface flex items-center justify-center">
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

export default function SmugMugAuthCard({ api, mode, onCancel, onChangeKey }: SmugMugAuthCardProps) {
  const { t } = useTranslation();
  const [key, setKey] = useState('');
  const [secret, setSecret] = useState('');
  const [verifier, setVerifier] = useState('');
  const [busy, setBusy] = useState<'save' | 'connect' | 'finish' | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (action: 'save' | 'connect' | 'finish', work: () => Promise<void>) => {
    setBusy(action);
    setError(null);
    try {
      await work();
    } catch (err) {
      setError(displayError(err, t('publish.errors.localFile')));
    } finally {
      setBusy(null);
    }
  };

  const errorLine = error && (
    <Text variant={TextVariants.small} color={TextColors.error}>
      {error}
    </Text>
  );

  if (mode === 'configure') {
    const canSave = key.trim() !== '' && secret.trim() !== '' && busy === null;
    return (
      <div className="space-y-4">
        <Text variant={TextVariants.heading}>{t('publish.smugmug.setup.heading')}</Text>
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
            if (canSave) run('save', () => api.setCredentials(key.trim(), secret.trim()));
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

          <div className="flex items-start gap-2 bg-surface rounded-md p-3">
            <ShieldCheck size={16} className="shrink-0 mt-0.5 text-text-secondary" />
            <Text variant={TextVariants.small}>{t('publish.smugmug.setup.privacy')}</Text>
          </div>

          {errorLine}

          <div className="flex gap-2">
            <Button type="submit" className="flex-1" disabled={!canSave}>
              {busy === 'save' ? <Loader size={16} className="animate-spin" /> : <KeyRound size={16} />}
              {busy === 'save' ? t('publish.smugmug.setup.saving') : t('publish.smugmug.setup.save')}
            </Button>
            {onCancel && (
              <Button type="button" className="bg-surface text-text-primary shadow-none" onClick={onCancel}>
                {t('publish.smugmug.setup.cancel')}
              </Button>
            )}
          </div>
        </form>
      </div>
    );
  }

  const { challenge } = api;
  const canFinish = challenge !== null && verifier.trim() !== '' && busy === null;

  return (
    <div className="space-y-4">
      <Text variant={TextVariants.heading}>{t('publish.smugmug.authorise.heading')}</Text>
      {challenge && <Text>{t(challenge.instructions_key as 'publish.smugmug.authInstructions')}</Text>}

      <Step number={1} title={t('publish.smugmug.authorise.stepOne')}>
        <Button
          className={challenge ? 'bg-surface text-text-primary shadow-none w-full' : 'w-full'}
          disabled={busy !== null}
          onClick={() => run('connect', challenge ? api.reopenAuthPage : api.beginAuth)}
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
          className="space-y-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (canFinish) run('finish', () => api.completeAuth(verifier.trim()));
          }}
        >
          <Input
            className="font-mono tracking-widest text-center"
            disabled={challenge === null}
            placeholder={t('publish.smugmug.authorise.verifierPlaceholder')}
            value={verifier}
            onChange={(e) => setVerifier(e.target.value)}
          />
          <Button type="submit" className="w-full" disabled={!canFinish}>
            {busy === 'finish' && <Loader size={16} className="animate-spin" />}
            {busy === 'finish' ? t('publish.smugmug.authorise.finishing') : t('publish.smugmug.authorise.finish')}
          </Button>
        </form>
      </Step>

      {errorLine}

      <div className="flex gap-4">
        {onChangeKey && (
          <button className="text-sm text-text-secondary hover:text-text-primary" onClick={onChangeKey}>
            {t('publish.smugmug.authorise.changeKey')}
          </button>
        )}
        {onCancel && (
          <button className="text-sm text-text-secondary hover:text-text-primary" onClick={onCancel}>
            {t('publish.smugmug.setup.cancel')}
          </button>
        )}
      </div>
    </div>
  );
}

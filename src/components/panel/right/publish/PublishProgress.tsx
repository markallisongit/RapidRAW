import { useTranslation } from 'react-i18next';
import { motion } from 'framer-motion';
import {
  AlertTriangle,
  Ban,
  CheckCircle,
  Circle,
  HelpCircle,
  Loader,
  RefreshCw,
  UploadCloud,
  XCircle,
} from 'lucide-react';

import Button from '../../../ui/Button';
import Text from '../../../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../../../types/typography';
import { ItemState, PublishStateApi, displayError } from './usePublishState';

const ITEM_ICONS: Record<ItemState, { icon: typeof Circle; className: string }> = {
  uploaded: { icon: UploadCloud, className: 'text-green-400' },
  updated: { icon: RefreshCw, className: 'text-green-400' },
  skipped: { icon: Circle, className: 'text-text-secondary' },
  failed: { icon: XCircle, className: 'text-red-400' },
  ambiguous: { icon: HelpCircle, className: 'text-yellow-400' },
};

const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

interface PublishProgressProps {
  api: PublishStateApi;
  destinationName: string;
}

export default function PublishProgress({ api, destinationName }: PublishProgressProps) {
  const { t } = useTranslation();
  const { phase, completed, total, items, summary, error } = api.session;
  const isRunning = phase === 'starting' || phase === 'running' || phase === 'cancelling';
  const percent = total > 0 ? (completed / total) * 100 : 0;

  const heading =
    phase === 'complete'
      ? t('publish.progress.completeHeading')
      : phase === 'cancelled'
        ? t('publish.progress.cancelledHeading')
        : phase === 'error'
          ? t('publish.progress.errorHeading')
          : `${t('publish.progress.heading')} · ${destinationName}`;

  return (
    <>
      <div className="grow overflow-y-auto p-3 space-y-6">
        <div className="space-y-2">
          <div className="flex justify-between items-baseline gap-2">
            <Text variant={TextVariants.heading}>{heading}</Text>
            {total > 0 && <Text variant={TextVariants.small}>{t('publish.progress.count', { completed, total })}</Text>}
          </div>
          <div className="w-full bg-bg-tertiary rounded-full h-1.5 border border-border-color">
            <div
              className={`h-1.5 rounded-full transition-all duration-500 ${phase === 'error' ? 'bg-red-500' : 'bg-accent'}`}
              style={{ width: `${phase === 'complete' ? 100 : percent}%` }}
            />
          </div>
          {isRunning && total === 0 && (
            <Text variant={TextVariants.small} className="italic">
              {t('publish.progress.preparing')}
            </Text>
          )}
        </div>

        {summary && (
          <div className="bg-surface rounded-md p-3 space-y-1">
            <Text color={TextColors.primary}>
              {t('publish.progress.summary', {
                uploaded: summary.uploaded,
                updated: summary.updated,
                skipped: summary.skipped,
              })}
            </Text>
            {summary.failed.length > 0 && (
              <Text color={TextColors.error}>
                {t('publish.progress.failedCount', { count: summary.failed.length })}
              </Text>
            )}
            {summary.ambiguous.length > 0 && (
              <Text variant={TextVariants.small}>
                {t('publish.progress.ambiguous', { count: summary.ambiguous.length })}
              </Text>
            )}
          </div>
        )}

        {error && (
          <div className="flex items-start gap-2 bg-red-500/10 rounded-md p-3">
            <AlertTriangle size={16} className="shrink-0 mt-0.5 text-red-400" />
            <Text color={TextColors.error}>{displayError(error, t('publish.errors.localFile'))}</Text>
          </div>
        )}

        {summary && summary.failed.length > 0 && (
          <div className="space-y-2">
            {summary.failed.map((failure) => (
              <div key={failure.path} className="pl-2 border-l-2 border-red-500/50">
                <Text color={TextColors.primary} weight={TextWeights.medium} className="truncate">
                  {fileName(failure.path)}
                </Text>
                <Text variant={TextVariants.small}>{displayError(failure.error, t('publish.errors.localFile'))}</Text>
              </div>
            ))}
          </div>
        )}

        {items.length > 0 && (
          <ul className="space-y-1">
            {items.map((item, index) => {
              const { icon: Icon, className } = ITEM_ICONS[item.state];
              return (
                <li key={`${index}-${item.file}`} className="flex items-center gap-2 min-w-0">
                  <Icon size={14} className={`shrink-0 ${className}`} />
                  <Text variant={TextVariants.small} color={TextColors.primary} className="truncate flex-1">
                    {item.file}
                  </Text>
                  <Text variant={TextVariants.small} className="shrink-0">
                    {t(`publish.progress.states.${item.state}`)}
                  </Text>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="p-3 border-t border-surface shrink-0">
        <motion.div
          whileTap={phase !== 'cancelling' ? { scale: 0.98 } : undefined}
          transition={{ type: 'spring', stiffness: 400, damping: 17 }}
          className="w-full"
        >
          {isRunning ? (
            <Button
              className={`group rounded-md h-11 w-full flex items-center text-md font-bold! justify-center ${
                phase === 'cancelling'
                  ? 'bg-yellow-500/20 text-yellow-400 shadow-none'
                  : 'bg-red-600/80 hover:bg-red-600 text-white'
              }`}
              disabled={phase !== 'running'}
              onClick={api.cancel}
              size="lg"
            >
              {phase === 'cancelling' ? (
                <>
                  <Loader size={18} className="animate-spin mr-2" /> {t('publish.actions.cancelling')}
                </>
              ) : phase === 'starting' ? (
                <>
                  <Loader size={18} className="animate-spin mr-2" /> {t('publish.actions.starting')}
                </>
              ) : (
                <>
                  <span className="flex items-center group-hover:hidden">
                    <Loader size={18} className="animate-spin mr-2" />
                    {total > 0 ? t('publish.progress.count', { completed, total }) : t('publish.progress.preparing')}
                  </span>
                  <span className="hidden items-center group-hover:flex">
                    <Ban size={18} className="mr-2" />
                    {t('publish.actions.cancel')}
                  </span>
                </>
              )}
            </Button>
          ) : (
            <Button
              className="rounded-md h-11 w-full flex items-center text-md font-bold! justify-center"
              onClick={api.dismissSession}
              size="lg"
            >
              <CheckCircle size={18} className="mr-2" /> {t('publish.actions.done')}
            </Button>
          )}
        </motion.div>
      </div>
    </>
  );
}

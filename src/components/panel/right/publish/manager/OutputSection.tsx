import { useTranslation } from 'react-i18next';
import { AlertTriangle, ArrowUpRight } from 'lucide-react';

import Dropdown from '../../../../ui/Dropdown';
import { ExportPreset } from '../../../../ui/ExportImportProperties';
import Text from '../../../../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../../../../types/typography';
import { LAST_USED_PRESET_ID, describeOutput, formatSupport } from '../output';
import { DestinationInfo } from '../usePublishState';
import ManagerSection from './ManagerSection';

interface OutputSectionProps {
  destination: DestinationInfo;
  presets: ExportPreset[];
  presetId: string | null;
  isRequested: boolean;
  onChange: (presetId: string) => void;
  onManagePresets: () => void;
}

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

export default function OutputSection({
  destination,
  presets,
  presetId,
  isRequested,
  onChange,
  onManagePresets,
}: OutputSectionProps) {
  const { t } = useTranslation();
  const choices = presets.filter((p) => p.id !== LAST_USED_PRESET_ID);
  const preset = choices.find((p) => p.id === presetId) ?? null;
  const support = preset ? formatSupport(destination, preset.fileFormat) : null;

  return (
    <ManagerSection title={t('publish.manager.output.heading')} isRequested={isRequested}>
      <div className="space-y-1">
        <Text as="span" variant={TextVariants.label} className="block">
          {t('publish.manager.output.presetLabel')}
        </Text>
        <Dropdown
          className="w-full"
          disabled={choices.length === 0}
          options={choices.map((p) => ({ label: p.name, value: p.id }))}
          placeholder={t('publish.manager.output.placeholder')}
          value={preset?.id ?? null}
          onChange={onChange}
        />
      </div>

      {preset ? (
        <Text color={TextColors.primary} weight={TextWeights.medium}>
          {describeOutput(preset, t)}
        </Text>
      ) : (
        <Warning>
          {choices.length === 0
            ? t('publish.manager.output.noPresets')
            : presetId === null
              ? t('publish.manager.output.unset', { destination: destination.display_name })
              : t('publish.manager.output.deleted')}
        </Warning>
      )}

      {support && !support.isAccepted && (
        <Warning>
          {t('publish.manager.output.unsupportedFormat', {
            destination: destination.display_name,
            formats: support.acceptedNames,
          })}
        </Warning>
      )}

      <button className="flex items-center gap-1 text-sm text-accent hover:underline" onClick={onManagePresets}>
        {t('publish.manager.output.manage')}
        <ArrowUpRight size={14} />
      </button>
    </ManagerSection>
  );
}

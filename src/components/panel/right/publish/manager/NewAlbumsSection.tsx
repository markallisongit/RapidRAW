import { useTranslation } from 'react-i18next';

import Dropdown from '../../../../ui/Dropdown';
import Text from '../../../../ui/Text';
import { TextVariants } from '../../../../../types/typography';
import { ContainerPrivacy, DestinationInfo } from '../usePublishState';
import ManagerSection from './ManagerSection';

interface NewAlbumsSectionProps {
  destination: DestinationInfo;
  privacy: ContainerPrivacy;
  isRequested: boolean;
  onChange: (privacy: ContainerPrivacy) => void;
}

export default function NewAlbumsSection({ destination, privacy, isRequested, onChange }: NewAlbumsSectionProps) {
  const { t } = useTranslation();

  return (
    <ManagerSection title={t('publish.manager.newAlbums.heading')} isRequested={isRequested}>
      <div className="space-y-1">
        <Text as="span" variant={TextVariants.label} className="block">
          {t('publish.manager.newAlbums.privacyLabel')}
        </Text>
        <Dropdown
          className="w-full"
          options={destination.capabilities.supported_privacy.map((value) => ({
            label: t(`publish.manager.newAlbums.privacy.${value}`),
            value,
          }))}
          value={privacy}
          onChange={onChange}
        />
      </div>
      <Text variant={TextVariants.small}>
        {t('publish.manager.newAlbums.note', { destination: destination.display_name })}
      </Text>
    </ManagerSection>
  );
}

import 'i18next';
import en from '../i18n/locales/en.json';

import type { PublishTranslations } from '../components/panel/right/publish/publish.i18n';

declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'translation';
    resources: {
      // Publish strings are registered at runtime, so they are not in en.json.
      translation: typeof en & PublishTranslations;
    };
  }
}
